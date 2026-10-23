use flaragon::{config::*, hosts, platform, web};

fn config() -> Config {
    serde_json::from_value(serde_json::json!({
        "schema":1,"revision":1,"http_port":28080,"apache":"apache","apache_usage":"common",
        "default_php":"php83","php":[{"package":"php83","port":29000,"workers":2,"usage":"normal"}],
        "mysql":[],"redis":null,"projects":[{"id":"cake","directory":"www/cake","document_root":"webroot","php":null,"enabled":true,"domain":"cake.localhost"}]
    })).unwrap()
}
fn layout() -> Layout {
    Layout::new(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "testwork/web-unit-{}",
            platform::random_token().unwrap()
        )),
    )
    .unwrap()
}

#[test]
fn domain_virtual_hosts_are_opt_in_and_paths_remain_available() {
    let mut c = config();
    let root = layout();
    c.validate(&root).unwrap();
    assert!(!c.projects[0].virtual_host);
    assert!(hosts::domains(&c).unwrap().is_empty());
    c.projects[0].virtual_host = true;
    assert_eq!(hosts::domains(&c).unwrap(), ["cake.localhost"]);
    c.projects[0].domain = "bad\nListen 8081".into();
    assert!(c.validate(&root).is_err());
    c.projects[0].virtual_host = false;
    c.validate(&root).unwrap();
}

#[test]
fn directory_entries_drive_dependencies_and_conflicts_without_projects() {
    let mut c = config();
    let root = layout();
    c.projects.clear();
    c.php[0].directory_entry = DirectoryEntry {
        enabled: true,
        port: 28074,
        directory: "www".into(),
        address: "127.0.0.2".into(),
    };
    c.validate(&root).unwrap();
    assert_eq!(c.startup_ids(true).unwrap(), ["php:php83", "apache"]);
    assert_eq!(c.http_listeners().unwrap().len(), 3);
    let mut changed = c.clone();
    changed.php[0].directory_entry.directory = "www/other".into();
    assert!(
        c.validate_settings_change(&changed, &["apache".into()])
            .is_err()
    );
    for port in [28080, 29000, 29001] {
        changed = c.clone();
        changed.php[0].directory_entry.port = port;
        assert!(changed.validate(&root).is_err());
    }
    c.php[0].usage = ServiceUsage::Disabled;
    assert!(c.startup_ids(true).is_err());
}

#[test]
fn paths_addresses_and_project_identifiers_are_validated() {
    let mut c = config();
    let root = layout();
    c.php[0].directory_entry.enabled = true;
    c.php[0].directory_entry.port = 28074;
    c.php[0].directory_entry.directory = "../outside".into();
    assert!(c.validate(&root).is_err());
    c.php[0].directory_entry.directory = "www".into();
    for address in [
        "host.local",
        "127.0.0.1\nListen 123",
        "224.0.0.1",
        "255.255.255.255",
    ] {
        c.http_address = address.into();
        assert!(c.validate(&root).is_err());
    }
    c.http_address = "127.0.0.1".into();
    let mut second = c.projects[0].clone();
    second.id = "CAKE".into();
    c.projects.push(second);
    assert!(c.validate(&root).is_err());
    c.projects.pop();
    c.projects[0].id = "_flaragon_health".into();
    assert!(c.validate(&root).is_err());
}

#[test]
fn navigation_only_lists_visible_enabled_projects_and_escapes_text() {
    let mut c = config();
    c.projects[0].name = "<img src=x onerror=alert(1)>".into();
    c.projects[0].group = "\"group\"".into();
    let page = web::navigation(&c);
    assert!(page.contains("&lt;img"));
    assert!(!page.contains("<img"));
    assert!(page.contains("href=\"/cake/\""));
    c.http_address = "192.168.1.50".into();
    assert!(!web::navigation(&c).contains("href=\"/cake/\""));
    c.projects[0].lan_access = true;
    assert!(web::navigation(&c).contains("href=\"/cake/\""));
    c.projects[0].listed = false;
    assert!(!web::navigation(&c).contains("href=\"/cake/\""));
    c.projects[0].listed = true;
    c.projects[0].enabled = false;
    assert!(!web::navigation(&c).contains("href=\"/cake/\""));
}
