use crate::{Result, ensure};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

fn connect(port: u16) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_secs(2),
    )?;
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
    let mut stream = connect(port)?;
    write!(stream, "*{}\r\n", args.len())?;
    for arg in args {
        write!(stream, "${}\r\n{}\r\n", arg.len(), arg)?;
    }
    let mut bytes = [0u8; 8192];
    let n = stream.read(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes[..n]).to_string();
    ensure!(!text.starts_with('-'), "Redis 拒绝操作：{}", text.trim());
    Ok(text)
}
pub fn mysql_greeting(port: u16) -> Result<String> {
    let mut stream = connect(port)?;
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
