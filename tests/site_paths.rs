use flaragon::{config::*, platform, web};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[test]
fn local_path_syntax_accepts_drive_directories_without_filesystem_access() {
    let base = Path::new(r"D:\environment");
    assert_eq!(
        local_path(base, Path::new(r"E:\业务 项目\cake")).unwrap(),
        PathBuf::from(r"E:\业务 项目\cake")
    );
    assert_eq!(
        local_path(base, Path::new(r"www\.\cake")).unwrap(),
        base.join(r"www\cake")
    );
    for input in [
        r"E:relative",
        r"\rooted",
        r"\\server\share",
        r"\\?\E:\site",
        r"\\.\C:",
        r"../sibling",
        r"www\NUL",
        r"www\file:stream",
        r"www\trailing.",
        "www\nListen 80",
        "www\"bad",
    ] {
        assert!(
            local_path(base, Path::new(input)).is_err(),
            "accepted {input}"
        );
    }
}

#[test]
fn business_directories_can_be_outside_environment_but_managed_storage_cannot() {
    let test_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/site-paths-{}",
        platform::random_token().unwrap()
    ));
    let layout = Layout::new(test_root.join("environment")).unwrap();
    let business = test_root.join("业务 项目");
    // Both siblings remain inside the repository's development boundary.
    layout.site_path(&business).unwrap();
    fs::create_dir_all(business.join("webroot")).unwrap();
    fs::write(business.join("webroot/index.php"), "<?php echo 'fixture';").unwrap();
    assert!(layout.path(&business).is_err());
    assert!(layout.site_path(&business).is_ok());
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "schema":1,"revision":1,"http_port":28080,"apache":"apache","default_php":"php83",
        "php":[{"package":"php83","port":29000,"workers":2,"directory_entry":{"enabled":true,"port":28081,"directory":business}}],
        "mysql":[],"redis":null,"projects":[{"id":"cake","directory":business,"document_root":"webroot","enabled":true,"virtual_host":true,"domain":"cake.localhost"}]
    })).unwrap();
    config.validate(&layout).unwrap();
    let routes = web::routes(
        &layout,
        &config,
        Path::new("runtime/generated/test"),
        "fixture",
    )
    .unwrap();
    assert!(routes.contains(&format!(
        "Alias \"/cake/\" \"{}/\"",
        apache_path(&business.join("webroot")).unwrap()
    )));
    assert!(routes.contains(&format!(
        "DocumentRoot \"{}\"",
        apache_path(&business).unwrap()
    )));
    layout.save(&config).unwrap();
    assert_eq!(layout.load().unwrap().projects[0].directory, business);
    assert_eq!(
        fs::read_to_string(business.join("webroot/index.php")).unwrap(),
        "<?php echo 'fixture';"
    );
    assert_eq!(fs::read_dir(&business).unwrap().count(), 1);
    for relative in [r"..\other", r"\rooted", r"D:relative", r"D:\other"] {
        config.projects[0].document_root = relative.into();
        assert!(config.validate(&layout).is_err());
    }
    config.projects[0].document_root = "".into();
    assert_eq!(layout.project_root(&config.projects[0]).unwrap(), business);
    if let Some(boundary) = layout.development_root() {
        let outside = boundary.with_file_name("flaragon-outside-boundary");
        assert!(
            layout
                .site_path(outside)
                .unwrap_err()
                .to_string()
                .contains("开发隔离范围")
        );
    }
}
