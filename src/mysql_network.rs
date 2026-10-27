//! MySQL listener policy; older servers accept only one bind address.
use crate::{Context, Result, ensure};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

pub fn dual_loopback(version: &str) -> Result<bool> {
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法识别 MySQL 监听能力：{version}"))?;
    ensure!(parts.len() == 3, "无法识别 MySQL 监听能力：{version}");
    Ok((parts[0], parts[1], parts[2]) >= (8, 0, 13))
}

pub fn bind_address(version: &str) -> Result<&'static str> {
    Ok(if dual_loopback(version)? {
        "127.0.0.1,::1"
    } else {
        "127.0.0.1"
    })
}

pub fn listeners(port: u16, dual: bool) -> Vec<SocketAddr> {
    let mut addresses = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
    if dual {
        addresses.push(SocketAddr::from((Ipv6Addr::LOCALHOST, port)));
    }
    addresses
}

pub fn preflight(port: u16, dual: bool) -> Result<()> {
    // Bind only; never connect to an unknown listener. Launch still checks ownership
    // because another process can take a port after this preflight releases it.
    let _listeners = listeners(port, dual)
        .into_iter()
        .map(|address| {
            TcpListener::bind(address)
                .with_context(|| format!("MySQL 监听 {address} 不可用，未访问占用服务"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(())
}
