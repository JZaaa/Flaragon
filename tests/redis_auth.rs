use flaragon::{platform::secrets, protocol, redis_config};
use std::{
    io::{BufReader, Write},
    net::TcpListener,
    thread,
};

#[test]
fn passwords_follow_redis_quoting_and_last_value() {
    assert!(redis_config::password("requirepass\u{b}example").is_err());
    assert_eq!(
        redis_config::password("requirepass example\u{b}").unwrap(),
        b"example\x0b"
    );
    assert_eq!(
        redis_config::password("requirepass a\u{a0}").unwrap(),
        "a\u{a0}".as_bytes()
    );
    assert_eq!(
        redis_config::password(
            "# requirepass ignored\nrequirepass first\nREQUIREPASS \"a b\\n\\x41\\\\\\\"中文\"\n"
        )
        .unwrap(),
        "a b\nA\\\"中文".as_bytes()
    );
    assert_eq!(
        redis_config::password("requirepass 'a\\\'b\\c'").unwrap(),
        b"a'b\\c"
    );
    assert!(
        redis_config::password("requirepass first\nrequirepass \"\"")
            .unwrap()
            .is_empty()
    );
    for bad in [
        "requirepass",
        "requirepass=secret",
        "requirepass secret extra",
        "requirepass \"secret",
        "requirepass \"secret\"suffix",
        "requirepass \"\\x00\"",
        "\"requirepass\" secret",
    ] {
        let error = redis_config::password(bad).unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }
}

#[test]
fn protected_records_roundtrip_and_reject_damage() {
    let secret = flaragon::platform::random_token().unwrap();
    let mut encrypted = secrets::seal(secret.as_bytes()).unwrap();
    assert!(
        !encrypted
            .windows(secret.len())
            .any(|w| w == secret.as_bytes())
    );
    assert_eq!(secrets::open(&encrypted).unwrap(), secret.as_bytes());
    let last = encrypted.len() - 1;
    encrypted[last] ^= 1;
    assert!(secrets::open(&encrypted).is_err());
    assert_eq!(secrets::open(b"legacy").unwrap(), b"legacy");
    assert!(
        secrets::open(&secrets::seal(b"").unwrap())
            .unwrap()
            .is_empty()
    );
}

fn read_command(reader: &mut BufReader<std::net::TcpStream>) -> Vec<Vec<u8>> {
    use std::io::{BufRead, Read};
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let count: usize = line[1..].trim().parse().unwrap();
    (0..count)
        .map(|_| {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let size: usize = line[1..].trim().parse().unwrap();
            let mut body = vec![0; size + 2];
            reader.read_exact(&mut body).unwrap();
            body.truncate(size);
            body
        })
        .collect()
}

#[test]
fn authenticated_commands_wait_for_complete_auth_and_do_not_leak_rejection() {
    for accepted in [true, false] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = thread::spawn(move || {
            use std::io::Read;
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            assert_eq!(
                read_command(&mut reader),
                vec![b"AUTH".to_vec(), b"secret\r\n\0".to_vec()]
            );
            if accepted {
                for byte in b"+OK\r\n" {
                    reader.get_mut().write_all(&[*byte]).unwrap();
                }
                assert_eq!(read_command(&mut reader), vec![b"PING".to_vec()]);
                for byte in b"+PONG\r\n" {
                    reader.get_mut().write_all(&[*byte]).unwrap();
                }
            } else {
                reader
                    .get_mut()
                    .write_all(b"-WRONGPASS secret\r\n")
                    .unwrap();
                assert_eq!(reader.read(&mut [0]).unwrap(), 0);
            }
        });
        let result = protocol::redis_authenticated(port, b"secret\r\n\0", &["PING"]);
        if accepted {
            assert_eq!(result.unwrap(), "+PONG\r\n");
        } else {
            assert!(!result.unwrap_err().to_string().contains("secret"));
        }
        worker.join().unwrap();
    }
}
