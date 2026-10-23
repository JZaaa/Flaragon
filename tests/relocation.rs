use flaragon::{config::*, engine::Engine, platform, relocation, service_config};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn environment() -> Layout {
    Layout::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testwork/relocation-unit")
            .join(platform::random_token().unwrap()),
    )
    .unwrap()
}

fn old_root(layout: &Layout) -> PathBuf {
    layout.root.with_file_name("旧目录 空格")
}

fn seed(layout: &Layout, old: &Path) {
    let mut config = Config::default();
    config.php.push(PhpSpec {
        package: "php-fixture".into(),
        port: 19111,
        workers: 1,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry {
            directory: old.join("www/中文 网站"),
            ..DirectoryEntry::default()
        },
    });
    config.default_php = Some("php-fixture".into());
    atomic_write(
        &layout.path("config/app.json").unwrap(),
        &serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
    let head = format!(
        "[PHP]\nsys_temp_dir=\"{}/runtime/temp\"\n",
        apache_path(&layout.root).unwrap()
    );
    let candidate = layout.path("runtime/generated/php.ini").unwrap();
    service_config::prepare(
        layout,
        "php:php-fixture",
        Kind::Php,
        &head,
        "",
        "memory_limit=321M\n",
        &candidate,
        None,
    )
    .unwrap();
    let file = service_config::commit(layout, "php:php-fixture", &candidate).unwrap();
    for path in [&file, &file.with_extension("managed.json")] {
        let text = fs::read_to_string(path).unwrap();
        // JSON strings use forward slashes in this fixture's managed fields.
        let text = relocation::rebase_text(&text, &layout.root, old);
        atomic_write(path, text.as_bytes()).unwrap();
    }
    let state = serde_json::json!({"records":[], "token":"0123456789abcdef0123456789abcdef",
        "applied": config, "applied_files": { old.join("config/services/php/php-fixture/php.ini").to_str().unwrap(): [] }});
    atomic_write(
        &layout.path("runtime/state/processes.json").unwrap(),
        &serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn legacy_paths_relocate_without_accessing_old_directory_and_stay_unchanged_on_restart() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let report = relocation::check(&layout).unwrap();
    assert!(report.moved);
    let config = layout.load().unwrap();
    assert_eq!(
        config.php[0].directory_entry.directory,
        Path::new("www/中文 网站")
    );
    let file = service_config::path(&layout, "php:php-fixture").unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("memory_limit=321M"));
    assert!(text.contains(&apache_path(&layout.root).unwrap()));
    assert!(!text.contains("旧目录"));
    service_config::validate_edit(&layout, "php:php-fixture", Kind::Php, &text).unwrap();
    assert!(
        Engine::new(layout.clone())
            .unwrap()
            .snapshot()
            .applied_revision
            .is_none()
    );
    let files = [
        file,
        layout.path("config/paths.json").unwrap(),
        layout.path("config/app.json").unwrap(),
        layout.path("runtime/state/processes.json").unwrap(),
    ];
    let before: Vec<_> = files
        .iter()
        .map(|p| {
            (
                fs::read(p).unwrap(),
                fs::metadata(p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    assert_eq!(relocation::check(&layout).unwrap().updated_files, 0);
    layout.save(&config).unwrap();
    let after: Vec<_> = files
        .iter()
        .map(|p| {
            (
                fs::read(p).unwrap(),
                fs::metadata(p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    assert_eq!(before, after);
}

#[test]
fn path_prefixes_respect_boundaries_slashes_case_and_unicode() {
    let old = Path::new("D:/测试 Dir/Flaragon");
    let new = Path::new("E:/另一个 名称/flaragon_dev");
    let text = "root=\"d:\\测试 dir\\flaragon\\data\"\nother=\"D:/测试 Dir/Flaragon-other/data\"\nspaced=\"D:/测试 Dir/Flaragon copy/data\"\nurl=https://host/D:/测试 Dir/Flaragon/data\n";
    let next = relocation::rebase_text(text, old, new);
    assert!(next.contains("E:\\另一个 名称\\flaragon_dev\\data"));
    assert!(next.contains("D:/测试 Dir/Flaragon-other/data"));
    assert!(next.contains("D:/测试 Dir/Flaragon copy/data"));
    assert!(next.contains("https://host/D:/测试 Dir/Flaragon/data"));
}

#[test]
fn current_external_site_is_preserved_and_internal_site_is_saved_relative() {
    let layout = environment();
    let mut config = Config::default();
    let sibling = layout.root.parent().unwrap().join("独立站点");
    config.php.push(PhpSpec {
        package: "php-fixture".into(),
        port: 19111,
        workers: 1,
        usage: ServiceUsage::Normal,
        directory_entry: DirectoryEntry {
            directory: sibling.clone(),
            ..DirectoryEntry::default()
        },
    });
    layout.save(&config).unwrap();
    relocation::check(&layout).unwrap();
    assert_eq!(
        layout.load().unwrap().php[0].directory_entry.directory,
        sibling
    );
    config.php[0].directory_entry.directory = layout.root.join("www");
    layout.save(&config).unwrap();
    assert_eq!(
        layout.load().unwrap().php[0].directory_entry.directory,
        Path::new("www")
    );
}

#[test]
fn moving_environment_with_live_record_refuses_without_changing_config() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let file = layout.path("runtime/state/processes.json").unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut identity = platform::Process::open(std::process::id())
        .unwrap()
        .identity;
    identity.executable = old.join("bin/php/test/php-cgi.exe");
    state["records"] = serde_json::json!([{"identity":identity}]);
    atomic_write(&file, &serde_json::to_vec(&state).unwrap()).unwrap();
    let config = layout.path("config/app.json").unwrap();
    let before = fs::read(&config).unwrap();
    let error = relocation::check(&layout).err().unwrap().to_string();
    assert!(error.contains("尚未退出"), "{error}");
    assert_eq!(fs::read(config).unwrap(), before);
    assert!(!layout.path("config/paths.json").unwrap().exists());
}

#[test]
fn interrupted_path_update_restores_before_retry() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let file = layout.path("config/app.json").unwrap();
    let before = fs::read(&file).unwrap();
    atomic_write(&file, b"interrupted write").unwrap();
    let journal = serde_json::json!([{"path":"config/app.json", "before":before, "after":b"interrupted write"}]);
    atomic_write(
        &layout.path("runtime/state/path-update.json").unwrap(),
        &platform::secrets::seal(&serde_json::to_vec(&journal).unwrap()).unwrap(),
    )
    .unwrap();
    assert!(relocation::check(&layout).unwrap().moved);
    assert!(layout.load().is_ok());
    assert!(
        !layout
            .path("runtime/state/path-update.json")
            .unwrap()
            .exists()
    );
}

#[test]
fn redis_password_is_preserved_on_relocation_and_backups_are_protected() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let password = format!(
        "{}/{}",
        apache_path(&old).unwrap(),
        platform::random_token().unwrap()
    );
    let head = format!(
        "dir \"{}/data/redis\"\n",
        apache_path(&layout.root).unwrap()
    );
    let candidate = layout.path("runtime/generated/redis.conf").unwrap();
    service_config::prepare(
        &layout,
        "redis",
        Kind::Redis,
        &head,
        "",
        &format!("requirepass \"{password}\"\n"),
        &candidate,
        None,
    )
    .unwrap();
    let file = service_config::commit(&layout, "redis", &candidate).unwrap();
    for path in [&file, &file.with_extension("managed.json")] {
        let text = fs::read_to_string(path).unwrap();
        atomic_write(
            path,
            relocation::rebase_text(&text, &layout.root, &old).as_bytes(),
        )
        .unwrap();
    }
    assert!(relocation::check(&layout).unwrap().moved);
    let text = fs::read_to_string(file).unwrap();
    assert_eq!(
        flaragon::redis_config::password(&text).unwrap(),
        password.as_bytes()
    );
    assert!(text.contains(&apache_path(&layout.root).unwrap()));
    for entry in fs::read_dir(layout.path("runtime/path-backups").unwrap()).unwrap() {
        let data = fs::read(entry.unwrap().path().join("changes.json")).unwrap();
        assert!(data.starts_with(b"FLARAGON-DPAPI-1\0"));
        assert!(
            !data
                .windows(password.len())
                .any(|w| w == password.as_bytes())
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&platform::secrets::open(&data).unwrap())
                .is_ok()
        );
    }
}

#[test]
fn corrupt_runtime_and_escaping_journal_are_rejected_without_writes() {
    let layout = environment();
    let file = layout.path("runtime/state/processes.json").unwrap();
    atomic_write(&file, b"broken").unwrap();
    assert!(relocation::check(&layout).is_err());
    assert!(!layout.path("config/paths.json").unwrap().exists());
    let journal =
        serde_json::json!([{"path":"config/services/../../outside", "before":null, "after":[]}]);
    atomic_write(
        &layout.path("runtime/state/path-update.json").unwrap(),
        &serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(relocation::check(&layout).is_err());
    assert_eq!(fs::read(file).unwrap(), b"broken");
}

#[test]
fn interrupted_update_does_not_overwrite_concurrent_user_edit() {
    let layout = environment();
    let path = layout.path("config/app.json").unwrap();
    atomic_write(&path, b"user edit").unwrap();
    let journal =
        serde_json::json!([{"path":"config/app.json", "before":b"previous", "after":b"candidate"}]);
    atomic_write(
        &layout.path("runtime/state/path-update.json").unwrap(),
        &serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(relocation::check(&layout).is_err());
    assert_eq!(fs::read(path).unwrap(), b"user edit");
}

#[test]
fn invalid_native_config_is_rejected_before_any_paths_are_committed() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let path = service_config::path(&layout, "php:php-fixture").unwrap();
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\nport=22\nauto_prepend_file=../outside.php\n");
    atomic_write(&path, text.as_bytes()).unwrap();
    let app = layout.path("config/app.json").unwrap();
    let before = fs::read(&app).unwrap();
    assert!(relocation::check(&layout).is_err());
    assert_eq!(fs::read(app).unwrap(), before);
    assert_eq!(fs::read(path).unwrap(), text.as_bytes());
    assert!(!layout.path("config/paths.json").unwrap().exists());
}

#[test]
fn native_backups_are_not_treated_as_active_configuration() {
    let layout = environment();
    let old = old_root(&layout);
    seed(&layout, &old);
    let path = service_config::path(&layout, "php:php-fixture")
        .unwrap()
        .with_file_name("php.backup.ini");
    let text = format!("backup at {}", old.display());
    atomic_write(&path, text.as_bytes()).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    assert!(relocation::check(&layout).unwrap().moved);
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
}
