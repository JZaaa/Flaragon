use flaragon::{
    config::*,
    mysql_data::{self, State},
    platform,
};
use std::{fs, path::PathBuf};

fn fixture() -> (Layout, MysqlSpec, Package) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "testwork/mysql-data-{}",
        platform::random_token().unwrap()
    ));
    let layout = Layout::prepare(root).unwrap();
    layout.initialize().unwrap();
    let spec = MysqlSpec {
        id: "mysql-main".into(),
        name: "main".into(),
        series: "5.7".into(),
        package: "mysql-test".into(),
        port: 23357,
        usage: ServiceUsage::Normal,
        reuse_data: false,
    };
    let p = Package {
        id: spec.package.clone(),
        name: "test".into(),
        kind: Kind::Mysql,
        directory: layout.root.join("bin/mysql/test"),
        executable: layout.root.join("bin/mysql/test/bin/mysqld.exe"),
        version: "5.7.44".into(),
        architecture: "x64".into(),
        php_thread_safe: None,
        error: None,
    };
    (layout, spec, p)
}
fn data(layout: &Layout, spec: &MysqlSpec) {
    let dir = spec.data_path(layout).unwrap();
    fs::create_dir_all(dir.join("mysql")).unwrap();
    fs::write(dir.join("mysql/user.frm"), b"fixture").unwrap();
    fs::write(dir.join("ibdata1"), b"data sentinel").unwrap();
}

#[test]
fn directory_states_do_not_modify_data_or_create_binding() {
    let (l, s, p) = fixture();
    assert_eq!(
        mysql_data::inspect(&l, &s, &p).unwrap().state,
        State::Missing
    );
    fs::create_dir_all(s.data_path(&l).unwrap()).unwrap();
    assert_eq!(mysql_data::inspect(&l, &s, &p).unwrap().state, State::Empty);
    fs::write(s.data_path(&l).unwrap().join("unknown"), b"keep").unwrap();
    assert_eq!(
        mysql_data::inspect(&l, &s, &p).unwrap().state,
        State::Incomplete
    );
    data(&l, &s);
    let before = mysql_data::stamp(&l, &s.data_path(&l).unwrap()).unwrap();
    let report = mysql_data::inspect(&l, &s, &p).unwrap();
    assert_eq!(report.state, State::Unbound);
    assert!(report.can_adopt);
    assert_eq!(before, mysql_data::stamp(&l, &report.path).unwrap());
    assert!(!s.binding_path(&l).unwrap().exists());
}

#[test]
fn legacy_and_structured_binding_preserve_strict_build_identity() {
    let (l, s, p) = fixture();
    data(&l, &s);
    for bytes in [
        serde_json::to_vec("mysql-test:5.7.44").unwrap(),
        mysql_data::binding_bytes(&p, "adopted").unwrap(),
    ] {
        fs::write(s.binding_path(&l).unwrap(), bytes).unwrap();
        assert_eq!(mysql_data::inspect(&l, &s, &p).unwrap().state, State::Bound);
        let mut other = p.clone();
        other.id = "other".into();
        let report = mysql_data::inspect(&l, &s, &other).unwrap();
        assert_eq!(report.state, State::Mismatch);
        assert!(!report.can_adopt);
    }
    fs::write(s.binding_path(&l).unwrap(), b"invalid").unwrap();
    assert_eq!(
        mysql_data::inspect(&l, &s, &p).unwrap().state,
        State::Invalid
    );
}

#[test]
fn incomplete_initialization_and_adoption_are_distinguished() {
    let (l, s, p) = fixture();
    data(&l, &s);
    let dir = s.data_path(&l).unwrap();
    fs::write(dir.join(mysql_data::INITIALIZING), b"pending").unwrap();
    let r = mysql_data::inspect(&l, &s, &p).unwrap();
    assert_eq!(r.state, State::Initializing);
    assert!(!r.can_adopt);
    fs::remove_file(dir.join(mysql_data::INITIALIZING)).unwrap();
    fs::write(
        dir.join(mysql_data::ADOPTING),
        mysql_data::binding_bytes(&p, "adopted").unwrap(),
    )
    .unwrap();
    let r = mysql_data::inspect(&l, &s, &p).unwrap();
    assert_eq!(r.state, State::AdoptionPending);
    assert!(r.can_adopt);
    let mut other = p.clone();
    other.version = "5.7.43".into();
    assert!(mysql_data::inspect(&l, &s, &other).is_err());
}

#[test]
fn conflicting_version_evidence_and_occupied_data_refuse_adoption() {
    let (l, s, p) = fixture();
    data(&l, &s);
    let dir = s.data_path(&l).unwrap();
    fs::write(dir.join("mysql_upgrade_info"), b"5.7.43\n").unwrap();
    assert!(!mysql_data::inspect(&l, &s, &p).unwrap().can_adopt);
    fs::write(dir.join("mysql_upgrade_info"), b"5.7.44\n").unwrap();
    assert!(mysql_data::inspect(&l, &s, &p).unwrap().can_adopt);
    let lock = mysql_data::exclusive_data_file(&l, &s).unwrap();
    assert!(mysql_data::exclusive_data_file(&l, &s).is_err());
    drop(lock);
    assert!(mysql_data::exclusive_data_file(&l, &s).is_ok());
}

#[test]
fn review_stamp_detects_added_removed_and_edited_files() {
    let (l, s, _) = fixture();
    data(&l, &s);
    let dir = s.data_path(&l).unwrap();
    let before = mysql_data::stamp(&l, &dir).unwrap();
    fs::write(dir.join("new-file"), b"new").unwrap();
    assert_ne!(before, mysql_data::stamp(&l, &dir).unwrap());
    fs::remove_file(dir.join("mysql/user.frm")).unwrap();
    assert_ne!(before, mysql_data::stamp(&l, &dir).unwrap());
    fs::write(dir.join("ibdata1"), b"changed data fixture").unwrap();
    assert_ne!(before, mysql_data::stamp(&l, &dir).unwrap());
}

#[test]
fn binding_publication_never_overwrites_existing_marker() {
    let (l, s, p) = fixture();
    data(&l, &s);
    mysql_data::publish_binding(&l, &s, &p, "adopted").unwrap();
    let marker = s.binding_path(&l).unwrap();
    let before = fs::read(&marker).unwrap();
    let modified = fs::metadata(&marker).unwrap().modified().unwrap();
    assert!(mysql_data::publish_binding(&l, &s, &p, "initialized").is_err());
    assert_eq!(fs::read(&marker).unwrap(), before);
    assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), modified);
}
