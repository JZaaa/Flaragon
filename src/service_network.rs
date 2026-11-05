//! Native listener selection and local control endpoints for owned database processes.
use crate::{Context, Result, ensure};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, ToSocketAddrs};

pub fn mysql(help: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let value = help
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("bind-address")).then(|| fields.collect::<Vec<_>>().join(""))
        })
        .context("MySQL 未返回有效 bind-address 配置")?;
    let mut result = Vec::new();
    for value in value.split(',') {
        if value == "*" {
            result.push((Ipv6Addr::UNSPECIFIED, port).into());
        } else {
            result.extend(
                (value, port)
                    .to_socket_addrs()
                    .context("无法解析 MySQL bind-address")?,
            );
        }
    }
    ensure!(!result.is_empty(), "MySQL bind-address 不能为空");
    result.sort();
    result.dedup();
    Ok(result)
}

pub fn redis(text: &str, port: u16) -> Result<Vec<(SocketAddr, bool)>> {
    let mut result = vec![
        ((Ipv4Addr::UNSPECIFIED, port).into(), false),
        ((Ipv6Addr::UNSPECIFIED, port).into(), true),
    ];
    for line in text.lines() {
        if !crate::redis_config::directive(line).eq_ignore_ascii_case("bind") {
            continue;
        }
        let arguments = crate::redis_config::arguments(line.as_bytes())?;
        result.clear();
        for value in arguments.iter().skip(1) {
            let value = std::str::from_utf8(value)?;
            let optional = value.starts_with('-');
            let ip: IpAddr = value
                .trim_start_matches('-')
                .parse()
                .context("Redis bind 需要有效 IP 地址")?;
            result.push((SocketAddr::new(ip, port), optional));
        }
    }
    ensure!(
        !result.is_empty(),
        "Redis bind 没有 TCP 监听地址，无法管理此实例"
    );
    Ok(result)
}

pub fn preflight(addresses: &[(SocketAddr, bool)]) -> Result<Vec<SocketAddr>> {
    let mut listeners = Vec::new();
    let mut selected = Vec::new();
    for (address, optional) in addresses {
        match TcpListener::bind(address) {
            Ok(listener) => {
                listeners.push(listener);
                selected.push(*address);
            }
            Err(e) if *optional && e.raw_os_error() == Some(10049) => (),
            Err(e) => {
                return Err(e).with_context(|| format!("监听 {address} 不可用，未连接占用服务"));
            }
        }
    }
    ensure!(!selected.is_empty(), "没有可用的本机监听地址");
    Ok(selected)
}

pub fn control(address: SocketAddr) -> SocketAddr {
    match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => (Ipv4Addr::LOCALHOST, address.port()).into(),
        IpAddr::V6(ip) if ip.is_unspecified() => (Ipv6Addr::LOCALHOST, address.port()).into(),
        _ => address,
    }
}
