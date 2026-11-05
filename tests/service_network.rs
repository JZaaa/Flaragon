use flaragon::{platform, service_network as network};
use std::net::{Ipv4Addr, TcpListener};

#[test]
fn native_addresses_and_control_endpoints_follow_user_selection() {
    for value in ["0.0.0.0", "127.0.0.2", "::1", "127.0.0.1,::1", "*"] {
        let addresses = network::mysql(&format!("bind-address {value}\nport 3307"), 3307).unwrap();
        assert!(!addresses.is_empty());
        assert!(
            addresses
                .iter()
                .all(|a| !network::control(*a).ip().is_unspecified())
        );
    }
    let addresses = network::redis("bind 127.0.0.1\nbind \"0.0.0.0\" -::1\n", 6390).unwrap();
    assert_eq!(addresses[0].0.ip().to_string(), "0.0.0.0");
    assert!(addresses[1].1);
    assert!(network::redis("bind invalid", 6390).is_err());
    assert!(network::redis("bind", 6390).is_err());
}

#[test]
fn preflight_rejects_conflicts_without_connecting_and_ownership_is_address_specific() {
    let listener = TcpListener::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).unwrap();
    let address = listener.local_addr().unwrap();
    assert!(network::preflight(&[(address, false)]).is_err());
    assert!(network::preflight(&[(address, true)]).is_err());
    let process = platform::Process::open(std::process::id()).unwrap();
    assert!(process.owns_listener(address).unwrap());
    assert!(
        !process
            .owns_listener((Ipv4Addr::LOCALHOST, address.port()).into())
            .unwrap()
    );
    drop(listener);
    assert_eq!(network::preflight(&[(address, false)]).unwrap(), [address]);
}
