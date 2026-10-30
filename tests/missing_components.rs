use flaragon::{config::*, engine::Engine, platform};
use std::{fs, path::PathBuf};

fn fixture() -> Layout {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/missing-{}",
        platform::random_token().unwrap()
    ));
    let layout = Layout::new(root).unwrap();
    layout.initialize().unwrap();
    layout
}

fn php(id: &str, port: u16) -> PhpSpec {
    PhpSpec {
        package: id.into(),
        port,
        workers: 1,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry::default(),
    }
}

fn project(id: &str, php: Option<&str>, enabled: bool) -> Project {
    serde_json::from_value(serde_json::json!({
        "id":id, "directory":format!("www/{id}"), "document_root":"", "php":php, "enabled":enabled
    }))
    .unwrap()
}

#[test]
fn manual_removal_persists_and_preserves_files_and_other_missing_services() {
    let layout = fixture();
    let mut config = Config {
        default_php: Some("php-gone".into()),
        apache: Some("apache-gone".into()),
        php: vec![php("php-gone", 19000)],
        redis: Some(RedisSpec {
            package: "redis-gone".into(),
            port: 16379,
            usage: ServiceUsage::Disabled,
        }),
        ..Config::default()
    };
    config.php[0].directory_entry = DirectoryEntry {
        enabled: true,
        port: 18074,
        ..DirectoryEntry::default()
    };
    config.mysql.push(MysqlSpec {
        id: "database".into(),
        package: "mysql-gone".into(),
        name: "demo".into(),
        series: "8.0".into(),
        port: 13306,
        usage: ServiceUsage::Normal,
        reuse_data: false,
    });
    layout.save(&config).unwrap();
    let sentinel = layout.path("data/mysql/8.0/demo/keep.bin").unwrap();
    fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    fs::write(&sentinel, b"keep data").unwrap();
    let before = fs::metadata(&sentinel).unwrap().modified().unwrap();
    let legacy = layout.path("config/overrides/keep.ini").unwrap();
    fs::write(&legacy, b"keep configuration").unwrap();
    let mut engine = Engine::new(layout.clone()).unwrap();
    engine.scan().unwrap();
    assert!(engine.snapshot().rows.iter().all(|r| r.missing));
    engine.remove_missing_service("php:php-gone").unwrap();
    assert!(engine.config.php.is_empty() && engine.config.default_php.is_none());
    assert!(engine.config.apache.is_some() && engine.config.redis.is_some());
    let saved = fs::read(layout.path("config/app.json").unwrap()).unwrap();
    assert!(engine.remove_missing_service("php:php-gone").is_err());
    assert_eq!(
        saved,
        fs::read(layout.path("config/app.json").unwrap()).unwrap()
    );
    for id in ["apache", "redis", "mysql:database"] {
        engine.remove_missing_service(id).unwrap();
    }
    engine.scan().unwrap();
    assert!(engine.snapshot().rows.is_empty());
    engine.start_all().unwrap();
    assert!(
        Engine::new(layout.clone())
            .unwrap()
            .service_ids()
            .is_empty()
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep data");
    assert_eq!(before, fs::metadata(&sentinel).unwrap().modified().unwrap());
    assert_eq!(fs::read(legacy).unwrap(), b"keep configuration");
}

#[test]
fn explicit_disabled_and_default_project_references_require_switching_first() {
    let layout = fixture();
    let config = Config {
        default_php: Some("php-gone".into()),
        php: vec![php("php-gone", 19000), php("php-other", 19010)],
        projects: vec![
            project("explicit", Some("php-gone"), false),
            project("inherited", None, true),
        ],
        ..Config::default()
    };
    layout.save(&config).unwrap();
    let mut engine = Engine::new(layout.clone()).unwrap();
    engine.scan().unwrap();
    let before = fs::read(layout.path("config/app.json").unwrap()).unwrap();
    let error = engine
        .remove_missing_service("php:php-gone")
        .unwrap_err()
        .to_string();
    assert!(error.contains("explicit") && error.contains("inherited"));
    assert_eq!(
        before,
        fs::read(layout.path("config/app.json").unwrap()).unwrap()
    );
    let mut next = engine.config.clone();
    next.projects[0].php = Some("php-other".into());
    engine.apply(next).unwrap();
    assert!(
        engine
            .remove_missing_service("php:php-gone")
            .unwrap_err()
            .to_string()
            .contains("inherited")
    );
    let mut next = engine.config.clone();
    next.default_php = Some("php-other".into());
    engine.apply_settings(next).unwrap();
    engine.remove_missing_service("php:php-gone").unwrap();
    assert_eq!(engine.config.projects.len(), 2);
    assert!(!engine.config.projects[0].enabled);
    assert_eq!(engine.config.default_php.as_deref(), Some("php-other"));
}

#[test]
fn fresh_discovery_rejects_restored_or_broken_existing_package() {
    let layout = fixture();
    let config = Config {
        php: vec![php("php-returned", 19000)],
        ..Config::default()
    };
    layout.save(&config).unwrap();
    let mut engine = Engine::new(layout.clone()).unwrap();
    engine.scan().unwrap();
    assert!(engine.snapshot().rows[0].missing);
    fs::create_dir_all(layout.path("bin/php/returned").unwrap()).unwrap();
    let before = fs::read(layout.path("config/app.json").unwrap()).unwrap();
    assert!(
        engine
            .remove_missing_service("php:php-returned")
            .unwrap_err()
            .to_string()
            .contains("组件仍存在或已恢复")
    );
    assert_eq!(
        before,
        fs::read(layout.path("config/app.json").unwrap()).unwrap()
    );
}

#[test]
fn running_services_and_web_dependencies_cannot_be_removed() {
    let config = Config {
        apache: Some("apache-gone".into()),
        php: vec![php("php-gone", 19000)],
        ..Config::default()
    };
    let next = config.without_service("php:php-gone").unwrap();
    for running in ["php:php-gone", "apache"] {
        assert!(
            config
                .validate_settings_change(&next, &[running.into()])
                .is_err()
        );
    }
}
