use crate::{Result, ensure};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

fn connect(port: u16) -> Result<TcpStream> {
    connect_at(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}
fn connect_at(address: SocketAddr) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.set_write_timeout(Some(Duration::from_secs(4)))?;
    Ok(stream)
}
pub fn http(port: u16, host: &str, path: &str) -> Result<(u16, String)> {
    ensure!(
        !host.contains(['\n', '\r']) && path.starts_with('/') && !path.contains(['\n', '\r']),
        "HTTP 参数无效"
    );
    let mut stream = connect(port)?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    stream.take(1024 * 1024).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| crate::anyhow!("HTTP 响应格式无效"))?;
    let status = head.split_whitespace().nth(1).unwrap_or("0").parse()?;
    Ok((status, body.to_owned()))
}
pub fn redis(port: u16, args: &[&str]) -> Result<String> {
    redis_authenticated(port, &[], args)
}
pub fn redis_authenticated(port: u16, password: &[u8], args: &[&str]) -> Result<String> {
    redis_authenticated_at((Ipv4Addr::LOCALHOST, port).into(), password, args)
}
pub fn redis_authenticated_at(
    address: SocketAddr,
    password: &[u8],
    args: &[&str],
) -> Result<String> {
    let mut stream = connect_at(address)?;
    if !password.is_empty() {
        redis_write(&mut stream, &[b"AUTH", password])?;
    }
    let mut reader = BufReader::new(stream);
    if !password.is_empty() {
        let reply = redis_reply(&mut reader, 0)?;
        ensure!(
            reply == b"+OK\r\n",
            "Redis 认证失败，请检查实例密码；未发送控制命令"
        );
    }
    redis_write(
        reader.get_mut(),
        &args.iter().map(|s| s.as_bytes()).collect::<Vec<_>>(),
    )?;
    let reply = redis_reply(&mut reader, 0)?;
    if reply.starts_with(b"-LOADING ") {
        crate::bail!("Redis 尚在加载数据（LOADING）");
    }
    ensure!(
        !reply.starts_with(b"-"),
        "Redis 拒绝操作，请核对认证及实例状态"
    );
    ensure!(
        !reply.is_empty() || args.first() == Some(&"SHUTDOWN"),
        "Redis 响应为空"
    );
    Ok(String::from_utf8_lossy(&reply).into_owned())
}
fn redis_write(stream: &mut TcpStream, args: &[&[u8]]) -> Result<()> {
    write!(stream, "*{}\r\n", args.len())?;
    for arg in args {
        write!(stream, "${}\r\n", arg.len())?;
        stream.write_all(arg)?;
        stream.write_all(b"\r\n")?;
    }
    Ok(())
}
fn redis_reply(reader: &mut impl BufRead, depth: usize) -> Result<Vec<u8>> {
    ensure!(depth < 16, "Redis 响应层级过深");
    let mut result = Vec::new();
    reader.take(8193).read_until(b'\n', &mut result)?;
    if result.is_empty() {
        return Ok(result);
    }
    ensure!(
        result.len() <= 8192 && result.ends_with(b"\r\n"),
        "Redis 响应格式无效"
    );
    let count =
        || -> Result<i64> { Ok(std::str::from_utf8(&result[1..result.len() - 2])?.parse()?) };
    match result[0] {
        b'+' | b'-' | b':' => (),
        b'$' => {
            let size = count()?;
            ensure!((-1..=1024 * 1024).contains(&size), "Redis 响应过大");
            if size >= 0 {
                let mut body = vec![0; size as usize + 2];
                reader.read_exact(&mut body)?;
                ensure!(body.ends_with(b"\r\n"), "Redis 响应格式无效");
                result.extend(body);
            }
        }
        b'*' => {
            let size = count()?;
            ensure!((-1..=4096).contains(&size), "Redis 响应过大");
            for _ in 0..size {
                result.extend(redis_reply(reader, depth + 1)?);
                ensure!(result.len() <= 1024 * 1024, "Redis 响应过大");
            }
        }
        _ => crate::bail!("Redis 响应格式无效"),
    }
    Ok(result)
}
pub fn mysql_greeting(port: u16) -> Result<String> {
    mysql_greeting_at((Ipv4Addr::LOCALHOST, port).into())
}
pub fn mysql_greeting_at(address: SocketAddr) -> Result<String> {
    let mut stream = connect_at(address)?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = header[0] as usize | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
    ensure!(len > 2 && len < 65536, "MySQL 握手长度无效");
    let mut bytes = vec![0u8; len];
    stream.read_exact(&mut bytes)?;
    ensure!(bytes[0] == 10, "MySQL 握手版本无效");
    let end = bytes[1..].iter().position(|b| *b == 0).unwrap_or(0) + 1;
    Ok(String::from_utf8_lossy(&bytes[1..end]).to_string())
}
