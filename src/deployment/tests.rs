use super::*;

fn fixture() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/deployment-space path-{}",
        platform::random_token().unwrap()
    ));
    assert!(std::env::var_os("FLARAGON_DEV_ROOT").is_some());
    fs::create_dir_all(&root).unwrap();
    root
}

fn package(ver: &str, mode: Mode) -> Package {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for name in [
        "Flaragon.exe",
        "lib/Scintilla.dll",
        "lib/Lexilla.dll",
        "licenses/notice.txt",
    ] {
        files.insert(name.into(), format!("fixture {name} {ver}").into_bytes());
    }
    let manifest = Manifest {
        product: "Flaragon".into(),
        version: ver.into(),
        release_id: format!("{:0<32}", ver.replace(".", "")),
        architecture: "x86_64-pc-windows-msvc".into(),
        format: 1,
        config_schema: 1,
        minimum_version: "0.2.0".into(),
        files: files
            .iter()
            .map(|(n, b)| (n.clone(), b.len() as u64))
            .collect(),
        remove: BTreeSet::new(),
        excluded_versions: BTreeSet::new(),
    };
    files.insert(
        RELEASE.into(),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    Package {
        mode,
        manifest,
        files,
        repair_base: None,
        update_base: None,
    }
}

fn pass(_: &Path, _: &str) -> Result<()> {
    Ok(())
}

#[test]
fn previous_release_only_rejects_skips_without_changes_and_resumes() {
    let make = |ver: &str, minimum: &str, mode| {
        let mut p = package(ver, mode);
        p.manifest.minimum_version = minimum.into();
        p.files
            .insert(RELEASE.into(), serde_json::to_vec(&p.manifest).unwrap());
        p
    };
    let root = fixture();
    let old = make("0.0.2", "0.0.1", Mode::Install);
    run(&old, &root, pass, None).unwrap();
    let next = make("0.0.4", "0.0.3", Mode::Update);
    let before = read_program(&root);
    let error = run(&next, &root, pass, None).unwrap_err().to_string();
    assert!(error.contains("上一正式版本 0.0.3"));
    assert_eq!(before, read_program(&root));
    assert!(!root.join(ACTIVE).exists());

    let previous = make("0.0.3", "0.0.1", Mode::Update);
    run(&previous, &root, pass, None).unwrap();
    assert!(run(&next, &root, pass, Some(1)).is_err());
    run(&next, &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap(), next.manifest);
    assert!(run(&next, &root, pass, None).is_err());
    assert!(run(&previous, &root, pass, None).is_err());
}

#[test]
fn previous_release_is_exact_across_version_gaps_and_legacy_range_is_preserved() {
    let mut next = package("0.1.0", Mode::Update).manifest;
    next.minimum_version = "0.0.4".into();
    assert!(upgrade_allowed(&package("0.0.4", Mode::Install).manifest, &next).is_ok());
    for ver in ["0.0.3", "0.0.5", "0.1.0", "0.2.0"] {
        assert!(upgrade_allowed(&package(ver, Mode::Install).manifest, &next).is_err());
    }
    next.version = "0.0.3".into();
    next.minimum_version = "0.0.1".into();
    for ver in ["0.0.1", "0.0.2"] {
        assert!(upgrade_allowed(&package(ver, Mode::Install).manifest, &next).is_ok());
    }
}

fn repair_package() -> Package {
    let mut repair = package("0.2.0", Mode::Repair);
    repair.repair_base = Some(repair.manifest.clone());
    repair.manifest.release_id = "f".repeat(32);
    repair.files.get_mut("Flaragon.exe").unwrap()[0] = b'R';
    repair.files.insert(
        RELEASE.into(),
        serde_json::to_vec(&repair.manifest).unwrap(),
    );
    repair
}

fn overwrite_package() -> Package {
    let mut p = repair_package();
    p.mode = Mode::Update;
    p.update_base = p.repair_base.take();
    p
}

#[test]
fn bound_same_version_update_rejects_other_builds_and_repeat_without_writes() {
    let root = fixture();
    first(&root);
    let before = read_program(&root);
    let mut wrong = overwrite_package();
    wrong.update_base.as_mut().unwrap().release_id = "a".repeat(32);
    assert!(run(&wrong, &root, pass, None).is_err());
    wrong.update_base = None;
    assert!(run(&wrong, &root, pass, None).is_err());
    assert_eq!(before, read_program(&root));
    assert!(!root.join(ACTIVE).exists());
    let next = overwrite_package();
    let identity = installation(&root).unwrap();
    assert!(
        inspect_with(&next, &root, pass)
            .unwrap()
            .description(&next)
            .contains("同版本覆盖升级")
    );
    run(&next, &root, pass, None).unwrap();
    assert_eq!(installation(&root).unwrap(), identity);
    let after = read_program(&root);
    assert!(run(&next, &root, pass, None).is_err());
    assert!(run(&package("0.2.0", Mode::Update), &root, pass, None).is_err());
    assert_eq!(after, read_program(&root));
}

#[test]
fn interrupted_bound_update_requires_exact_base_and_payload() {
    let root = fixture();
    first(&root);
    let next = overwrite_package();
    assert!(run(&next, &root, pass, Some(1)).is_err());
    assert!(ensure_ready(&root).is_err());
    let mut wrong = overwrite_package();
    wrong.update_base = None;
    assert!(run(&wrong, &root, pass, None).is_err());
    wrong.update_base = next.update_base.clone();
    wrong.update_base.as_mut().unwrap().release_id = "a".repeat(32);
    assert!(run(&wrong, &root, pass, None).is_err());
    wrong.update_base = next.update_base.clone();
    wrong.files.get_mut("Flaragon.exe").unwrap()[0] = b'X';
    assert!(run(&wrong, &root, pass, None).is_err());
    run(&next, &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap(), next.manifest);
    assert!(ensure_ready(&root).is_ok());
}

#[test]
fn same_version_repair_requires_exact_base_and_preserves_installation() {
    let root = fixture();
    first(&root);
    let identity = installation(&root).unwrap();
    let repair = repair_package();
    let before = read_program(&root);
    let lock = platform::AppLock::acquire(&root).unwrap();
    assert!(run(&repair, &root, pass, None).is_err());
    assert_eq!(read_program(&root), before);
    drop(lock);
    let mut other = repair_package();
    other.repair_base.as_mut().unwrap().release_id = "a".repeat(32);
    assert!(run(&other, &root, pass, None).is_err());
    assert_eq!(read_program(&root), before);
    run(&repair, &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap(), repair.manifest);
    assert_eq!(installation(&root).unwrap(), identity);
    assert!(run(&repair, &root, pass, None).is_err());
    assert!(run(&package("0.2.0", Mode::Update), &root, pass, None).is_err());
    run(&package("0.3.0", Mode::Update), &root, pass, None).unwrap();
}

#[test]
fn interrupted_same_version_repair_requires_same_base_and_payload() {
    let root = fixture();
    first(&root);
    let repair = repair_package();
    assert!(run(&repair, &root, pass, Some(1)).is_err());
    assert!(ensure_ready(&root).is_err());
    let mut other = repair_package();
    other.repair_base.as_mut().unwrap().release_id = "a".repeat(32);
    assert!(run(&other, &root, pass, None).is_err());
    let mut other = repair_package();
    other.files.get_mut("Flaragon.exe").unwrap()[0] = b'X';
    assert!(run(&other, &root, pass, None).is_err());
    assert!(run(&repair, &root, fail, None).is_err());
    run(&repair, &root, pass, None).unwrap();
    assert!(ensure_ready(&root).is_ok());
    assert_eq!(installed(&root).unwrap(), repair.manifest);
}

#[test]
fn repair_archive_requires_baseline_and_does_not_enable_regular_same_version_update() {
    let repair = repair_package();
    let encode = |files: &BTreeMap<String, Vec<u8>>| {
        let mut archive = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, name, data.as_slice())
                .unwrap();
        }
        archive.into_inner().unwrap()
    };
    assert!(Package::from_archive(Mode::Repair, &encode(&repair.files)).is_err());
    let mut files = repair.files.clone();
    files.insert(
        REPAIR_BASE.into(),
        serde_json::to_vec(repair.repair_base.as_ref().unwrap()).unwrap(),
    );
    let parsed = Package::from_archive(Mode::Repair, &encode(&files)).unwrap();
    assert_eq!(parsed.repair_base, repair.repair_base);
    assert!(!parsed.files.contains_key(REPAIR_BASE));
    assert!(Package::from_archive(Mode::Update, &encode(&files)).is_err());
    files.remove(REPAIR_BASE);
    files.insert(
        UPDATE_BASE.into(),
        serde_json::to_vec(repair.repair_base.as_ref().unwrap()).unwrap(),
    );
    let parsed = Package::from_archive(Mode::Update, &encode(&files)).unwrap();
    assert_eq!(parsed.update_base, repair.repair_base);
    assert!(!parsed.files.contains_key(UPDATE_BASE));
    assert!(Package::from_archive(Mode::Install, &encode(&files)).is_err());
    assert!(Package::from_archive(Mode::Repair, &encode(&files)).is_err());
    let mut changed = repair.manifest.clone();
    changed.version = "0.3.0".into();
    assert!(repair_allowed(repair.repair_base.as_ref().unwrap(), &changed).is_err());
    changed = repair.manifest.clone();
    changed.files.insert("licenses/new.txt".into(), 10);
    assert!(repair_allowed(repair.repair_base.as_ref().unwrap(), &changed).is_err());
}
fn fail(_: &Path, _: &str) -> Result<()> {
    bail!("self-check failed")
}

fn first(root: &Path) {
    run(&package("0.2.0", Mode::Install), root, pass, None).unwrap();
}
fn read_program(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let manifest = installed(root).unwrap();
    manifest
        .files
        .keys()
        .map(String::as_str)
        .chain(std::iter::once(RELEASE))
        .map(|name| (name.to_owned(), fs::read(root.join(name)).unwrap()))
        .collect()
}

#[test]
fn install_and_upgrade_preserve_user_files_and_do_not_start_services() {
    let root = fixture();
    first(&root);
    let config = serde_json::to_vec(&crate::config::Config::default()).unwrap();
    for (name, bytes) in [
        ("config/app.json", config.as_slice()),
        ("data/mysql/sample", b"database sentinel"),
        ("www/site/index.php", b"site sentinel"),
        ("bin/php/user-package.txt", b"component sentinel"),
        ("runtime/temp/session", b"session sentinel"),
    ] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
    }
    run(&package("0.3.0", Mode::Update), &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap().version, "0.3.0");
    assert_eq!(fs::read(root.join("config/app.json")).unwrap(), config);
    assert_eq!(
        fs::read(root.join("data/mysql/sample")).unwrap(),
        b"database sentinel"
    );
    assert_eq!(
        fs::read(root.join("www/site/index.php")).unwrap(),
        b"site sentinel"
    );
    assert_eq!(
        fs::read(root.join("bin/php/user-package.txt")).unwrap(),
        b"component sentinel"
    );
    assert_eq!(
        fs::read(root.join("runtime/temp/session")).unwrap(),
        b"session sentinel"
    );
    assert!(!root.join("runtime/state/processes.json").exists());
}

#[test]
fn running_directory_blocks_update_without_any_replacement() {
    let root = fixture();
    first(&root);
    let before = read_program(&root);
    let lock = platform::AppLock::acquire(&root).unwrap();
    let error = run(&package("0.3.0", Mode::Update), &root, pass, None).unwrap_err();
    assert!(error.to_string().contains("停止并退出"));
    assert_eq!(before, read_program(&root));
    drop(lock);
}

fn run(
    package: &Package,
    root: &Path,
    verify: fn(&Path, &str) -> Result<()>,
    fail_after: Option<usize>,
) -> Result<String> {
    deploy_with(package, root, None, verify, pass, fail_after)
}
#[test]
fn interrupted_update_only_repairs_forward_and_rejects_other_packages() {
    let root = fixture();
    first(&root);
    let next = package("0.3.0", Mode::Update);
    assert!(run(&next, &root, pass, Some(2)).is_err());
    assert!(root.join(ACTIVE).exists());
    assert!(platform::AppLock::acquire(&root).is_err());
    assert_ne!(
        fs::read(root.join("Flaragon.exe")).unwrap(),
        package("0.2.0", Mode::Install).files["Flaragon.exe"]
    );
    for ver in ["0.2.0", "0.4.0"] {
        assert!(run(&package(ver, Mode::Update), &root, pass, None).is_err());
    }
    let mut other = package("0.3.0", Mode::Update);
    other.files.get_mut("Flaragon.exe").unwrap()[0] = b'x';
    assert!(run(&other, &root, pass, None).is_err());
    assert!(run(&next, &root, fail, None).is_err());
    assert!(root.join(ACTIVE).exists());
    run(&next, &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap().version, "0.3.0");
    assert!(!root.join(ACTIVE).exists());
    assert!(!root.join(".maintenance/last.json").exists());
    assert!(platform::AppLock::acquire(&root).is_ok());
}
#[test]
fn failed_install_retries_same_payload_without_rollback() {
    let root = fixture();
    let pack = package("0.2.0", Mode::Install);
    assert!(run(&pack, &root, fail, None).is_err());
    assert!(root.join("Flaragon.exe").exists());
    assert!(root.join(ACTIVE).exists());
    run(&pack, &root, pass, None).unwrap();
    assert_eq!(installed(&root).unwrap().version, "0.2.0");
}
#[test]
fn rejects_same_version_downgrade_unknown_and_source_directories() {
    let root = fixture();
    first(&root);
    let before = read_program(&root);
    for ver in ["0.2.0", "0.1.0"] {
        assert!(run(&package(ver, Mode::Update), &root, pass, None).is_err());
    }
    assert_eq!(before, read_program(&root));
    let other = fixture();
    fs::write(other.join("user.txt"), b"keep").unwrap();
    assert!(run(&package("0.2.0", Mode::Install), &other, pass, None).is_err());
    assert!(run(&package("0.3.0", Mode::Update), &other, pass, None).is_err());
    assert_eq!(fs::read(other.join("user.txt")).unwrap(), b"keep");
    assert!(target_root(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
    assert!(target_root(&other.join("../escape")).is_err());
}

#[test]
fn rejects_unsupported_config_and_foreign_service_records_before_writing() {
    let root = fixture();
    first(&root);
    let before = read_program(&root);
    fs::create_dir_all(root.join("config")).unwrap();
    let mut config = crate::config::Config {
        schema: 99,
        ..Default::default()
    };
    fs::write(
        root.join("config/app.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    assert!(run(&package("0.3.0", Mode::Update), &root, pass, None).is_err());
    config.schema = 1;
    fs::write(
        root.join("config/app.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(root.join("runtime/state")).unwrap();
    fs::write(root.join("runtime/state/processes.json"), serde_json::to_vec(&serde_json::json!({"records":[{"identity":{"pid":42,"created":1,"executable":root.join("unowned.exe")}}]})).unwrap()).unwrap();
    assert!(run(&package("0.3.0", Mode::Update), &root, pass, None).is_err());
    assert_eq!(before, read_program(&root));
}

#[test]
fn archive_validation_rejects_extra_files_links_and_case_collisions() {
    fn tar_bytes(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(*kind);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }
    for entries in [
        vec![(
            "config/app.json",
            b"bad".as_slice(),
            tar::EntryType::Regular,
        )],
        vec![("licenses/link", b"".as_slice(), tar::EntryType::Symlink)],
        vec![
            ("licenses/a", b"a".as_slice(), tar::EntryType::Regular),
            ("licenses/A", b"b".as_slice(), tar::EntryType::Regular),
        ],
    ] {
        assert!(Package::from_archive(Mode::Install, &tar_bytes(&entries)).is_err());
    }
    assert!(program_file("licenses/../../config/app.json").is_err());
    assert!(program_file("licenses/CON.txt").is_err());
}

#[test]
fn explicit_removal_only_deletes_owned_files_and_resumes_idempotently() {
    let root = fixture();
    first(&root);
    let mut next = package("0.3.0", Mode::Update);
    next.manifest.files.remove("licenses/notice.txt");
    next.files.remove("licenses/notice.txt");
    next.manifest.remove.insert("licenses/notice.txt".into());
    next.manifest.remove.insert("licenses/user.txt".into());
    next.manifest.files.insert("licenses/new.txt".into(), 3);
    next.files
        .insert("licenses/new.txt".into(), b"new".to_vec());
    next.files
        .insert(RELEASE.into(), serde_json::to_vec(&next.manifest).unwrap());
    fs::write(root.join("licenses/user.txt"), b"keep").unwrap();
    assert!(run(&next, &root, fail, None).is_err());
    assert!(!root.join("licenses/notice.txt").exists());
    run(&next, &root, pass, None).unwrap();
    assert_eq!(fs::read(root.join("licenses/user.txt")).unwrap(), b"keep");
}
#[test]
fn directory_review_rejects_different_installation_and_missing_identity() {
    let a = fixture();
    let b = fixture();
    first(&a);
    first(&b);
    let next = package("0.9.0", Mode::Update);
    let before = read_program(&b);
    let review = inspect_with(&next, &a, pass).unwrap();
    assert!(deploy_with(&next, &b, Some(&review), pass, pass, None).is_err());
    assert_eq!(before, read_program(&b));
    fs::remove_file(a.join(INSTALLATION)).unwrap();
    assert!(run(&next, &a, pass, None).is_err());
    assert!(review_target(&next, &b).is_err()); // Plain text with an EXE name fails PE identification.
}
#[test]
fn changed_review_damaged_files_and_occupied_dll_block_replacement() {
    let root = fixture();
    first(&root);
    let next = package("0.3.0", Mode::Update);
    let review = inspect_with(&next, &root, pass).unwrap();
    let mut identity = installation(&root).unwrap();
    identity.id = platform::random_token().unwrap();
    fs::write(
        root.join(INSTALLATION),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    assert!(deploy_with(&next, &root, Some(&review), pass, pass, None).is_err());
    let before = read_program(&root);
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("lib/Scintilla.dll"))
        .unwrap();
    assert!(run(&next, &root, pass, None).is_err());
    drop(held);
    assert_eq!(before, read_program(&root));
    fs::write(root.join("lib/Scintilla.dll"), b"damaged").unwrap();
    assert!(run(&next, &root, pass, None).is_err());
}
#[test]
fn cross_version_update_preserves_installation_and_other_environment() {
    let a = fixture();
    let b = fixture();
    first(&a);
    first(&b);
    let identity = installation(&a).unwrap();
    let other = read_program(&b);
    let _lock = platform::AppLock::acquire(&b).unwrap();
    run(&package("1.10.0", Mode::Update), &a, pass, None).unwrap();
    assert_eq!(installation(&a).unwrap(), identity);
    assert_eq!(read_program(&b), other);
    let mut excluded = package("2.0.0", Mode::Update);
    excluded.manifest.excluded_versions.insert("1.10.0".into());
    assert!(run(&excluded, &a, pass, None).is_err());
}
#[test]
fn corrupted_sfx_is_rejected_before_target_access() {
    let root = fixture();
    let package = package("0.2.0", Mode::Install);
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in &package.files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&archive.into_inner().unwrap()).unwrap();
    let compressed = gzip.finish().unwrap();
    let mut sfx = b"stub".to_vec();
    sfx.extend_from_slice(&compressed);
    sfx.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    sfx.push(0);
    sfx.extend_from_slice(MAGIC);
    let path = root.join("fixture.exe");
    fs::write(&path, &sfx).unwrap();
    assert!(Package::open(&path).is_ok());
    let end = 4 + compressed.len();
    sfx[end - 8] ^= 1;
    fs::write(&path, &sfx).unwrap();
    assert!(Package::open(&path).is_err());
    fs::write(&path, &sfx[..sfx.len() - 1]).unwrap();
    assert!(Package::open(&path).is_err());
}
