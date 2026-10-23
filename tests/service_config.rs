use flaragon::{config::*, service_config as native};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn layout() -> Layout {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    Layout::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("testwork/native-config/{unique}")),
    )
    .expect("isolated layout")
}

#[test]
fn preserves_native_comments_sections_and_options_when_instance_settings_change() {
    let layout = layout();
    let id = "mysql:separate";
    let candidate = layout
        .path("runtime/generated/mysql.ini")
        .expect("candidate");
    native::prepare(
        &layout,
        id,
        Kind::Mysql,
        "[mysqld]\nport=13307\ncharacter-set-server=utf8mb4\n",
        "",
        "# 调优\nconnect_timeout=17\n[client]\ndefault-character-set=utf8mb4\n",
        &candidate,
        None,
    )
    .expect("prepare");
    let file = native::commit(&layout, id, &candidate).expect("commit");
    let text = native::read(&layout, id)
        .expect("read")
        .replace("connect_timeout=17", "# 保留注释\nconnect_timeout=23");
    native::validate_edit(&layout, id, Kind::Mysql, &text).expect("full native edit");
    fs::write(&file, &text).expect("edit");
    native::prepare(
        &layout,
        id,
        Kind::Mysql,
        "[mysqld]\nport=13309\n",
        "",
        "discarded seed",
        &candidate,
        None,
    )
    .expect("regenerate");
    assert_eq!(
        fs::read_to_string(&file).expect("disk"),
        text,
        "preflight cannot replace the live file"
    );
    native::commit(&layout, id, &candidate).expect("apply");
    let applied = native::read(&layout, id).expect("read applied");
    assert!(applied.contains("port=13309") && applied.contains("# 保留注释\nconnect_timeout=23"));
    assert!(applied.contains("[client]\ndefault-character-set=utf8mb4"));
    assert!(applied.contains("character-set-server=utf8mb4"));
    assert_ne!(
        native::path(&layout, id).expect("path"),
        native::path(&layout, "mysql:other").expect("other instance")
    );
}

#[test]
fn rejects_managed_changes_and_external_references_before_component_execution() {
    let layout = layout();
    let candidate = layout.path("runtime/generated/php.ini").expect("candidate");
    native::prepare(
        &layout,
        "php:test",
        Kind::Php,
        "[PHP]\nextension_dir=ext\n",
        "",
        "precision=12\n",
        &candidate,
        None,
    )
    .expect("prepare");
    native::commit(&layout, "php:test", &candidate).expect("commit");
    let text = native::read(&layout, "php:test").expect("read");
    assert!(
        native::validate_edit(
            &layout,
            "php:test",
            Kind::Php,
            &text.replace("extension_dir=ext", "extension_dir=elsewhere")
        )
        .is_err()
    );
    for (kind, text) in [
        (Kind::Php, "auto_prepend_file=D:/outside/script.php"),
        (Kind::Php, "zend_extension=../outside.dll"),
        (Kind::Mysql, "!include outside.ini"),
        (Kind::Mysql, "port=3306"),
        (Kind::Redis, "dir ../outside"),
        (Kind::Redis, "bind\u{b}127.0.0.1"),
        (Kind::Redis, "replicaof 127.0.0.1 6379"),
        (Kind::Redis, "user default on nopass"),
        (Kind::Redis, "rename-command SHUTDOWN disabled"),
        (Kind::Redis, "aclfile other.conf"),
        (Kind::Redis, "masterauth placeholder"),
        (Kind::Apache, "Include D:/outside/httpd.conf"),
        (Kind::Apache, "CustomLog |outside.exe common"),
    ] {
        assert!(
            native::validate_user(&layout, kind, text).is_err(),
            "accepted {text}"
        );
    }
    for (kind, text) in [
        (Kind::Php, "[PHP]\n; 原生参数\nprecision=12"),
        (Kind::Mysql, "[mysqld]\nconnect_timeout=17"),
        (Kind::Redis, "hz 13"),
        (Kind::Redis, "requirepass \"example /|${} 中文\""),
        (Kind::Apache, "ServerTokens Prod"),
    ] {
        native::validate_user(&layout, kind, text).expect("ordinary native options");
    }
}

#[test]
fn imports_package_template_once_and_keeps_existing_values_and_original_bytes() {
    let layout = layout();
    let directory = layout.path("bin/php/example").expect("package");
    fs::create_dir_all(&directory).expect("mkdir");
    let original = "; Native development template\n[PHP]\n; precision documentation\nprecision = 14\n; memory documentation\nmemory_limit = 128M\n;extension=pdo_mysql\n[Session]\nsession.cookie_path = /\n";
    fs::write(directory.join("php.ini-development"), original).expect("package template");
    let candidate = layout.path("runtime/generated/php.ini").expect("candidate");
    let id = "php:example";
    native::prepare(
        &layout,
        id,
        Kind::Php,
        "[PHP]\n",
        "",
        "; 我的设置\nprecision=12\n",
        &candidate,
        None,
    )
    .expect("short existing config");
    let file = native::commit(&layout, id, &candidate).expect("commit");
    let before = fs::read(&file).expect("before");
    assert!(native::needs_template(&layout, id).expect("migration status"));
    let template = flaragon::config_template::load(&layout, &directory, Kind::Php, "[PHP]\n")
        .expect("load template");
    native::prepare(
        &layout,
        id,
        Kind::Php,
        "[PHP]\n",
        "",
        "",
        &candidate,
        Some(&template),
    )
    .expect("template upgrade");
    assert_eq!(fs::read(&file).expect("unchanged active file"), before);
    native::commit(&layout, id, &candidate).expect("apply template");
    let full = native::read(&layout, id).expect("full");
    assert!(full.contains("; precision documentation\n; 实例自定义值见下方\n; precision = 14"));
    assert!(full.contains("memory_limit = 128M") && full.contains(";extension=pdo_mysql"));
    assert!(full.contains("; 我的设置\nprecision=12"));
    assert!(!native::needs_template(&layout, id).expect("imported"));
    native::prepare(
        &layout,
        id,
        Kind::Php,
        "[PHP]\n",
        "",
        "",
        &candidate,
        Some(&template),
    )
    .expect("repeat");
    assert_eq!(
        fs::read_to_string(&candidate).expect("repeat content"),
        full
    );
    assert_eq!(
        fs::read_to_string(directory.join("php.ini-development")).expect("original"),
        original
    );
    assert_eq!(
        fs::read(file.with_extension("backup")).expect("recoverable short file"),
        before
    );
}

#[test]
fn mysql_template_merge_preserves_group_scope_and_reports_missing_template() {
    let layout = layout();
    let directory = layout.path("bin/mysql/example").expect("package");
    let missing = flaragon::config_template::load(&layout, &directory, Kind::Mysql, "[mysqld]\n")
        .expect("missing template");
    assert_eq!(missing.source, "no-package-template");
    assert!(missing.text.contains("未附带配置模板"));
    let merged = flaragon::config_template::merge(
        Kind::Mysql,
        "[mysqld]\nconnect_timeout=10\n[client]\nconnect_timeout=20\n",
        "connect_timeout=17\n[client]\ndefault-character-set=utf8mb4\n",
    );
    assert!(merged.contains("; connect_timeout=10\n[client]\nconnect_timeout=20"));
    assert!(merged.contains("[mysqld]\nconnect_timeout=17\n[client]"));
}
