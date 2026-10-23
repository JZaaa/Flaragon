use flaragon::{config::*, engine::Engine, platform};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> Layout {
    Layout::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/policy-{name}-{}-{}",
        std::process::id(),
        platform::random_token().unwrap()
    )))
    .unwrap()
}
fn mysql(name: &str, series: &str, port: u16) -> MysqlSpec {
    MysqlSpec {
        id: format!("{}-{name}", series.replace('.', "-")),
        name: name.into(),
        series: series.into(),
        package: format!("mysql-{}", series.replace('.', "-")),
        port,
        usage: ServiceUsage::Normal,
        reuse_data: false,
    }
}
fn package(layout: &Layout, spec: &MysqlSpec) -> Package {
    Package {
        id: spec.package.clone(),
        name: spec.package.clone(),
        kind: Kind::Mysql,
        directory: layout.path("bin/mysql/fixture").unwrap(),
        executable: layout.path("bin/mysql/fixture/mysqld.exe").unwrap(),
        version: format!("{}.44", spec.series),
        architecture: "x64".into(),
        php_thread_safe: None,
        error: None,
    }
}

#[test]
fn startup_selection_respects_usage_and_checks_dependencies_before_starting() {
    let mut config = Config {
        apache: Some("apache".into()),
        apache_usage: ServiceUsage::Common,
        default_php: Some("php83".into()),
        ..Config::default()
    };
    config.php.push(PhpSpec {
        package: "php83".into(),
        port: 19000,
        workers: 2,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry::default(),
    });
    config.php.push(PhpSpec {
        package: "php74".into(),
        port: 19010,
        workers: 1,
        usage: ServiceUsage::Disabled,
        directory_entry: DirectoryEntry::default(),
    });
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
    config.mysql.push(mysql("demo", "5.7", 13307));
    let mut common = mysql("demo", "8.0", 13308);
    common.usage = ServiceUsage::Common;
    config.mysql.push(common);
    let mut disabled = mysql("off", "8.0", 13309);
    disabled.usage = ServiceUsage::Disabled;
    config.mysql.push(disabled);
    config.redis = Some(RedisSpec {
        package: "redis".into(),
        port: 16379,
        usage: ServiceUsage::Disabled,
    });
    assert_eq!(
        config.startup_ids(true).unwrap(),
        ["mysql:8-0-demo", "php:php83", "apache"]
    );
    assert_eq!(
        config.startup_ids(false).unwrap(),
        ["mysql:5-7-demo", "mysql:8-0-demo", "php:php83", "apache"]
    );
    config.php[0].usage = ServiceUsage::Disabled;
    assert!(
        config
            .startup_ids(true)
            .unwrap_err()
            .to_string()
            .contains("所需 PHP 已停用")
    );
    config.apache_usage = ServiceUsage::Disabled;
    assert_eq!(
        config.startup_ids(false).unwrap(),
        ["mysql:5-7-demo", "mysql:8-0-demo"]
    );
}

#[test]
fn data_directories_are_unique_per_series_and_case_insensitive() {
    assert_eq!(mysql_series("5.7.44").unwrap(), "5.7");
    assert_eq!(mysql_series("8.0.44").unwrap(), "8.0");
    assert_eq!(mysql_series("8.1.2").unwrap(), "8.1");
    assert!(mysql_series("../8.0").is_err());
    let layout = fixture("names");
    let mut config = Config {
        mysql: vec![
            mysql("demo", "5.7", 13307),
            mysql("demo", "8.0", 13308),
            mysql("demo", "8.4", 13309),
        ],
        ..Config::default()
    };
    config.validate(&layout).unwrap();
    assert!(
        config.mysql[0]
            .data_path(&layout)
            .unwrap()
            .ends_with("data/mysql/5.7/demo")
    );
    let mut duplicate = mysql("DEMO", "5.7", 13310);
    duplicate.id = "another-id".into();
    config.mysql.push(duplicate);
    assert!(
        config
            .validate(&layout)
            .unwrap_err()
            .to_string()
            .contains("不能被多个服务使用")
    );
    config.mysql.pop();
    config.mysql[0].name = "../escape".into();
    assert!(config.validate(&layout).is_err());
}

#[test]
fn running_settings_and_saved_database_identity_are_protected() {
    let config = Config {
        apache: Some("apache".into()),
        mysql: vec![mysql("demo", "5.7", 13307)],
        redis: Some(RedisSpec {
            package: "redis".into(),
            port: 16379,
            usage: ServiceUsage::Normal,
        }),
        ..Config::default()
    };
    let mut changed = config.clone();
    changed.mysql.clear();
    assert!(
        config
            .validate_settings_change(&changed, &["mysql:5-7-demo".into()])
            .is_err()
    );
    config.validate_settings_change(&changed, &[]).unwrap();
    let mut changed = config.clone();
    changed.mysql[0].usage = ServiceUsage::Disabled;
    assert!(
        config
            .validate_settings_change(&changed, &["mysql:5-7-demo".into()])
            .is_err()
    );
    config.validate_settings_change(&changed, &[]).unwrap();
    let mut changed = config.clone();
    changed.redis.as_mut().unwrap().port += 1;
    assert!(
        config
            .validate_settings_change(&changed, &["redis".into()])
            .is_err()
    );
    let mut changed = config.clone();
    changed.http_port += 1;
    assert!(
        config
            .validate_settings_change(&changed, &["apache".into()])
            .is_err()
    );
    let mut changed = config.clone();
    changed.mysql[0].name = "renamed".into();
    assert!(config.validate_settings_change(&changed, &[]).is_err());
}

#[test]
fn unchanged_database_directory_does_not_block_unrelated_settings() {
    let layout = fixture("inspection-scope");
    let mut spec = mysql("saved", "8.0", 13307);
    spec.usage = ServiceUsage::Disabled;
    let mut engine = Engine::new(layout.clone()).unwrap();
    engine.packages.push(package(&layout, &spec));
    engine.config.mysql.push(spec.clone());
    let dir = spec.data_path(&layout).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let marker = spec.binding_path(&layout).unwrap();
    fs::write(&marker, b"invalid binding fixture").unwrap();
    assert!(engine.inspect_mysql(&spec).is_err());

    let mut candidate = engine.config.clone();
    candidate.http_port += 1;
    candidate.mysql[0].port += 1;
    engine.apply_settings(candidate).unwrap();
    assert_eq!(engine.config.mysql[0].port, 13308);
    assert_eq!(fs::read(&marker).unwrap(), b"invalid binding fixture");

    let mut removed = engine.config.clone();
    removed.mysql.clear();
    engine.apply_settings(removed).unwrap();
    let mut readded = engine.config.clone();
    readded.mysql.push(spec);
    assert!(engine.apply_settings(readded).is_err());
    assert!(engine.config.mysql.is_empty());
    assert_eq!(fs::read(&marker).unwrap(), b"invalid binding fixture");
}

#[test]
fn removal_preserves_data_and_reuse_requires_matching_binding_and_confirmation() {
    let layout = fixture("reuse");
    let spec = mysql("demo", "5.7", 13307);
    let mut engine = Engine::new(layout.clone()).unwrap();
    let pkg = package(&layout, &spec);
    engine.packages.push(pkg.clone());
    engine.config.mysql.push(spec.clone());
    let dir = spec.data_path(&layout).unwrap();
    fs::create_dir_all(dir.join("mysql")).unwrap();
    fs::write(dir.join("sentinel"), b"preserve user data").unwrap();
    let marker = spec.binding_path(&layout).unwrap();
    fs::write(
        &marker,
        serde_json::to_vec(&format!("{}:{}", pkg.id, pkg.version)).unwrap(),
    )
    .unwrap();
    assert!(engine.inspect_mysql(&spec).unwrap());
    let mut removed = engine.config.clone();
    removed.mysql.clear();
    engine.apply_settings(removed).unwrap();
    assert_eq!(
        fs::read(dir.join("sentinel")).unwrap(),
        b"preserve user data"
    );
    assert!(marker.exists());
    let mut readded = engine.config.clone();
    readded.mysql.push(spec.clone());
    assert!(
        engine
            .apply_settings(readded)
            .unwrap_err()
            .to_string()
            .contains("确认使用历史数据")
    );
    fs::write(&marker, serde_json::to_vec("other:8.0.44").unwrap()).unwrap();
    assert!(
        engine
            .inspect_mysql(&spec)
            .unwrap_err()
            .to_string()
            .contains("其他 MySQL 构建")
    );
    fs::remove_file(marker).unwrap();
    assert!(
        engine
            .inspect_mysql(&spec)
            .unwrap_err()
            .to_string()
            .contains("无法确认版本")
    );
    assert_eq!(
        fs::read(dir.join("sentinel")).unwrap(),
        b"preserve user data"
    );
}
