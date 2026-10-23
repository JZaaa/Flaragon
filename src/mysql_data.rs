//! MySQL directory inspection and explicit adoption metadata; never initializes data.
use crate::{Context, Result, config::*, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Read, path::PathBuf, time::SystemTime};

pub const INITIALIZING: &str = ".flaragon-initializing.json";
pub const ADOPTING: &str = ".flaragon-adopting.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Missing,
    Empty,
    Bound,
    Unbound,
    Mismatch,
    Incomplete,
    Initializing,
    AdoptionPending,
    Invalid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub state: State,
    pub path: PathBuf,
    pub message: String,
    pub version_hint: Option<String>,
    pub can_adopt: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema: u32,
    pub package: String,
    pub version: String,
    pub origin: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredBinding {
    Legacy(String),
    Current(Binding),
}

fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    fs::File::open(path)?.take(4097).read_to_end(&mut data)?;
    ensure!(data.len() <= 4096, "数据目录标记超过大小限制");
    Ok(data)
}

pub fn inspect(layout: &Layout, spec: &MysqlSpec, package: &Package) -> Result<Report> {
    let path = spec.data_path(layout)?;
    let mut report = Report {
        state: State::Missing,
        path: path.clone(),
        message: "目录尚不存在，首次启动时初始化".into(),
        version_hint: None,
        can_adopt: false,
    };
    let mut set = |state, message: &str| {
        report.state = state;
        report.message = message.into();
    };
    if !path.exists() {
        return Ok(report);
    }
    ensure!(path.is_dir(), "数据路径不是目录");
    if layout.path(path.join(INITIALIZING))?.exists() {
        set(
            State::Initializing,
            "初始化未完成；保留数据，禁止重新初始化或接入",
        );
        return Ok(report);
    }
    let marker = spec.binding_path(layout)?;
    if marker.exists() {
        let parsed = serde_json::from_slice::<StoredBinding>(&read(&marker)?);
        let matches = match parsed {
            Ok(StoredBinding::Legacy(value)) => {
                value == format!("{}:{}", package.id, package.version)
            }
            Ok(StoredBinding::Current(b))
                if b.schema == 1 && matches!(b.origin.as_str(), "initialized" | "adopted") =>
            {
                b.package == package.id && b.version == package.version
            }
            _ => {
                set(
                    State::Invalid,
                    "绑定信息损坏或格式不支持；禁止覆盖，请核对备份",
                );
                return Ok(report);
            }
        };
        if !matches {
            set(
                State::Mismatch,
                "数据目录绑定其他 MySQL 构建，不能自动升级或降级",
            );
        } else if !layout.path(path.join("mysql"))?.is_dir() {
            set(
                State::Incomplete,
                "历史数据目录不完整，缺少 mysql 系统目录；拒绝重新初始化",
            );
        } else {
            set(State::Bound, "已绑定，所选 MySQL 构建匹配");
        }
        return Ok(report);
    }
    if fs::read_dir(&path)?.next().is_none() {
        set(State::Empty, "空目录，首次启动时初始化");
        return Ok(report);
    }
    let required = if spec.series == "5.7" {
        "mysql/user.frm"
    } else {
        "mysql.ibd"
    };
    if !layout.path(path.join("mysql"))?.is_dir()
        || !layout.path(path.join(required))?.is_file()
        || !layout.path(path.join("ibdata1"))?.is_file()
    {
        set(
            State::Incomplete,
            "已有数据无法确认版本，目录结构不完整或使用自定义表空间布局；请核对完整备份和原配置，不能自动接入",
        );
        return Ok(report);
    }
    if layout.path(path.join(ADOPTING))?.exists() {
        let pending: Binding = serde_json::from_slice(&read(&layout.path(path.join(ADOPTING))?)?)?;
        ensure!(
            pending.schema == 1
                && pending.package == package.id
                && pending.version == package.version,
            "未完成接入绑定其他构建，请使用原组件核对"
        );
        set(
            State::AdoptionPending,
            "上次接入未完成；正常停止后可重新检查并确认接入",
        );
    } else {
        set(
            State::Unbound,
            "已有数据未绑定，无法确认版本；保存服务后使用“接入数据”，禁止初始化或覆盖",
        );
    }
    let hint = layout.path(path.join("mysql_upgrade_info"))?;
    if hint.is_file() {
        let value = String::from_utf8(read(&hint)?)?.trim().to_owned();
        ensure!(
            value.len() <= 128 && !value.chars().any(char::is_control),
            "版本线索格式无效"
        );
        report.version_hint = Some(value.clone());
        if value != package.version {
            report.message = format!(
                "已有数据的升级记录为 {value}，与所选 {} 不同；请核实来源，接入不会执行升级",
                package.version
            );
            return Ok(report);
        }
    }
    report.can_adopt = true;
    Ok(report)
}

pub fn binding_bytes(package: &Package, origin: &str) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&Binding {
        schema: 1,
        package: package.id.clone(),
        version: package.version.clone(),
        origin: origin.into(),
    })?)
}

pub fn publish_binding(
    layout: &Layout,
    spec: &MysqlSpec,
    package: &Package,
    origin: &str,
) -> Result<()> {
    use std::io::Write;
    let directory = spec.data_path(layout)?;
    let temp = layout.path(directory.join(format!(
        ".flaragon-binding-{}.tmp",
        crate::platform::random_token()?
    )))?;
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&binding_bytes(package, origin)?)?;
        file.sync_all()?;
        drop(file);
        crate::platform::publish_new_file(&temp, &spec.binding_path(layout)?)
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp(pub BTreeMap<PathBuf, (u64, SystemTime, SystemTime)>);

/// Metadata-only review stamp, including every entry and rejecting all redirects.
pub fn stamp(layout: &Layout, root: &std::path::Path) -> Result<Stamp> {
    let mut result = BTreeMap::new();
    let mut pending = vec![(layout.path(root)?, 0)];
    let started = std::time::Instant::now();
    while let Some((path, depth)) = pending.pop() {
        ensure!(
            result.len() < 100_000 && depth <= 32 && started.elapsed().as_secs() < 15,
            "数据目录过大或层级过深，无法完成接入前复核"
        );
        let path = layout.path(path)?;
        let meta = fs::metadata(&path)?;
        result.insert(
            path.clone(),
            (meta.len(), meta.modified()?, meta.created()?),
        );
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push((entry?.path(), depth + 1));
            }
        } else {
            ensure!(meta.is_file(), "不支持的数据文件类型");
        }
    }
    Ok(Stamp(result))
}

pub fn exclusive_data_file(layout: &Layout, spec: &MysqlSpec) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(layout.path(spec.data_path(layout)?.join("ibdata1"))?)
        .context("数据文件被占用或无法独占读取；请确认所有使用此目录的 MySQL 已正常停止")
}
