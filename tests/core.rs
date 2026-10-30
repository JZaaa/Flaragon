use flaragon::{config::*, discovery, platform, service_config::validate_user};
use std::path::PathBuf;

fn layout() -> Layout {
    Layout::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testwork/unit"))
        .expect("isolated layout")
}
#[test]
fn rejects_path_escape_and_configuration_injection() {
    let layout = layout();
    assert!(layout.path("../outside").is_err());
    assert!(layout.path("D:/flaragon-other/data").is_err());
    assert!(layout.path("C:/other/data").is_err());
    assert!(layout.path("www/中文 空格/public").is_err());
    assert!(layout.path("www/site space/public").is_ok());
    for path in [
        "www/file:stream",
        "www/CON.txt",
        "www/dir. ",
        "D:relative",
        "\\rooted",
        "www/NUL",
    ] {
        assert!(layout.path(path).is_err(), "accepted {path}");
    }
    assert!(apache_path(&PathBuf::from("D:/flaragon/a\"b")).is_err());
    assert!(apache_path(&PathBuf::from("D:/flaragon/${bad}")).is_err());
}
#[test]
fn validates_worker_port_collisions_and_default_bindings() {
    let layout = layout();
    let mut config = Config::default();
    config.php.push(PhpSpec {
        package: "php-74".into(),
        port: 19000,
        workers: 2,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry::default(),
    });
    config.php.push(PhpSpec {
        package: "php-83".into(),
        port: 19001,
        workers: 2,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry::default(),
    });
    assert!(config.validate(&layout).is_err());
    config.php[1].port = 19002;
    config.default_php = Some("php-74".into());
    config.projects.push(Project {
        id: "site".into(),
        name: String::new(),
        group: String::new(),
        virtual_host: true,
        lan_access: false,
        listed: true,
        domain: "site.localhost".into(),
        directory: "www/site".into(),
        document_root: "public".into(),
        php: None,
        enabled: true,
    });
    assert!(config.validate(&layout).is_ok());
    config.default_php = Some("missing".into());
    assert!(config.validate(&layout).is_err());
    config.projects[0].php = Some("php-83".into());
    assert!(config.validate(&layout).is_ok());
    config.php[0].port = 65535;
    assert!(config.validate(&layout).is_err());
}
#[test]
fn legacy_project_mysql_field_is_ignored_without_changing_services() {
    let layout = layout();
    let mut saved = serde_json::to_value(Config::default()).unwrap();
    saved["php"] = serde_json::json!([
        {"package": "php-83", "port": 19000, "workers": 2}
    ]);
    saved["mysql"] = serde_json::json!([
        {"id": "database", "package": "mysql-80", "port": 13308, "enabled": false}
    ]);
    saved["projects"] = serde_json::json!([{
        "id": "site", "domain": "site.localhost", "directory": "www/site",
        "document_root": "public", "php": "php-83", "mysql": "missing-database",
        "enabled": true
    }]);
    let config: Config = serde_json::from_value(saved.clone()).unwrap();
    config.validate(&layout).unwrap();
    let serialized = serde_json::to_value(&config).unwrap();
    assert!(serialized["projects"][0].get("mysql").is_none());
    assert_eq!(serialized["mysql"][0]["id"], saved["mysql"][0]["id"]);
    assert_eq!(serialized["mysql"][0]["port"], saved["mysql"][0]["port"]);
    assert_eq!(
        serialized["mysql"][0]["package"],
        saved["mysql"][0]["package"]
    );
    assert_eq!(serialized["mysql"][0]["usage"], "normal");
    assert!(serialized["mysql"][0].get("enabled").is_none());
    let mut expected_project = saved["projects"][0].clone();
    expected_project.as_object_mut().unwrap().remove("mysql");
    for (key, value) in expected_project.as_object().unwrap() {
        assert_eq!(&serialized["projects"][0][key], value);
    }
    assert_eq!(serialized["projects"][0]["virtual_host"], false);
    assert_eq!(serialized["projects"][0]["lan_access"], false);
    let mut reloaded: Config = serde_json::from_value(serialized).unwrap();
    reloaded.mysql.clear();
    reloaded.validate(&layout).unwrap();
}

#[test]
fn rejects_external_service_configuration() {
    let layout = layout();
    for (kind, text) in [
        (Kind::Mysql, "datadir=D:/outside"),
        (Kind::Php, "auto_prepend_file=D:/outside"),
        (Kind::Php, "extension=../bad.dll"),
        (Kind::Redis, "include D:/outside.conf"),
        (Kind::Apache, "Include D:/outside.conf"),
    ] {
        assert!(validate_user(&layout, kind, text).is_err());
    }
    assert!(
        validate_user(
            &layout,
            Kind::Php,
            "memory_limit=128M\nextension=php_mysqli.dll"
        )
        .is_ok()
    );
    assert!(validate_user(&layout, Kind::Redis, "appendonly yes\nsave 60 1").is_ok());
}
#[test]
fn refuses_stale_process_identity() {
    let process = platform::Process::open(std::process::id()).expect("own process");
    let mut stale = process.identity.clone();
    stale.created += 1;
    assert!(platform::Process::recover(&stale).is_err());
    assert!(platform::Process::recover(&process.identity).is_ok());
}
#[test]
fn reports_listener_owner_without_connecting() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("isolated ephemeral port");
    let port = listener.local_addr().expect("port").port();
    assert_eq!(
        platform::listening_pid(port).expect("owner"),
        Some(std::process::id())
    );
}
#[test]
fn recognizes_real_component_version_strings() {
    assert_eq!(discovery::version("PHP 8.3.35 (cli)"), "8.3.35");
    assert_eq!(
        discovery::version("mysqld Ver 5.7.44 for Win64 on x86_64"),
        "5.7.44"
    );
    assert_eq!(
        discovery::version("Redis server v=7.4.3 sha=00000000:0"),
        "7.4.3"
    );
}

#[test]
fn hosts_merge_preserves_unmanaged_entries_and_rejects_conflicts() {
    let owner = "0123456789abcdef0123456789abcdef";
    let domains = vec!["site.test".to_string()];
    let original = "# unrelated comment\r\n127.0.0.1 localhost\r\n";
    let generated = flaragon::hosts::merge(original, &domains, owner).expect("mapping");
    assert!(generated.starts_with(original));
    assert!(generated.contains("127.0.0.1 site.test\r\n"));
    assert_eq!(
        flaragon::hosts::merge(&generated, &domains, owner).unwrap(),
        generated
    );
    assert_eq!(
        flaragon::hosts::merge(&generated, &[], owner).unwrap(),
        original
    );
    assert!(flaragon::hosts::merge("10.2.3.4 site.test\n", &domains, owner).is_err());
    assert!(
        flaragon::hosts::merge(&format!("# BEGIN FLARAGON {owner}\n"), &domains, owner).is_err()
    );
}

#[test]
fn hosts_file_transaction_is_confined_to_fixture_and_preserves_backup() {
    let layout = layout();
    let path = layout.path("hosts-fixture.txt").unwrap();
    let backup = layout.path("hosts-fixture.backup").unwrap();
    let original = b"# fixture only\r\n127.0.0.1 localhost\r\n";
    std::fs::write(&path, original).unwrap();
    let owner = "0123456789abcdef0123456789abcdef";
    flaragon::hosts::apply_file(&path, &backup, &["demo.test".into()], owner).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("127.0.0.1 demo.test")
    );
    let conflict = b"192.0.2.1 demo.test\n";
    std::fs::write(&path, conflict).unwrap();
    assert!(flaragon::hosts::apply_file(&path, &backup, &["demo.test".into()], owner).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), conflict);
}

#[test]
fn discovery_preserves_custom_folder_names_and_distinguishes_packages() {
    let layout = Layout::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/naming-unit-{}",
        platform::random_token().unwrap()
    )))
    .unwrap();
    for name in ["accounts 8.0", "reports 8.0"] {
        let directory = layout.path(format!("bin/mysql/{name}/nested/bin")).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        // Invalid executable fixtures exercise discovery labels without starting any service.
        std::fs::write(directory.join("mysqld.exe"), b"invalid executable fixture").unwrap();
    }
    let packages = discovery::scan(&layout).unwrap();
    assert_eq!(packages.len(), 2);
    assert_ne!(packages[0].id, packages[1].id);
    for name in ["accounts 8.0", "reports 8.0"] {
        let package = packages.iter().find(|p| p.label() == name).unwrap();
        assert!(package.error.is_some());
        assert!(package.directory.ends_with("nested"));
    }
}
