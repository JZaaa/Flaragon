use flaragon::{mysql_network, platform};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

#[test]
fn selects_only_loopback_addresses_supported_by_the_server() {
    for version in ["5.7.44", "8.0.12"] {
        assert_eq!(mysql_network::bind_address(version).unwrap(), "127.0.0.1");
    }
    for version in ["8.0.13", "8.0.44", "8.4.0", "8.4.8", "9.0.0"] {
        assert_eq!(
            mysql_network::bind_address(version).unwrap(),
            "127.0.0.1,::1"
        );
    }
    for version in ["", "8", "8.0", "8.0.x", "8.0.13.1"] {
        assert!(mysql_network::bind_address(version).is_err());
    }
    assert!(
        mysql_network::listeners(3308, true)
            .iter()
            .all(|a| a.ip().is_loopback())
    );
}

#[test]
fn rejects_ipv6_conflict_and_distinguishes_address_family_ownership() {
    let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let address = v6.local_addr().unwrap();
    let port = address.port();
    // An IPv4-only check would accept this port even though MySQL needs both.
    mysql_network::preflight(port, false).unwrap();
    assert!(
        mysql_network::preflight(port, true)
            .unwrap_err()
            .to_string()
            .contains("[::1]")
    );
    assert_eq!(
        platform::listening_pid_at(address).unwrap(),
        Some(std::process::id())
    );
    let process = platform::Process::open(std::process::id()).unwrap();
    assert!(process.owns_listener(address).unwrap());
    let v4_address = (Ipv4Addr::LOCALHOST, port).into();
    assert!(!process.owns_listener(v4_address).unwrap());
    let v4 = TcpListener::bind(v4_address).unwrap();
    assert!(process.owns_listener(v4_address).unwrap());
    assert!(mysql_network::preflight(port, false).is_err());
    drop(v4);
    drop(v6);
    mysql_network::preflight(port, true).unwrap();
}
