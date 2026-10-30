//! Startup path checks use only files in the current environment, never the previous location.
use crate::{Context, Result, config::*, ensure, platform::Identity};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const LOCATION: &str = "config/paths.json";
const STATE: &str = "runtime/state/processes.json";
const JOURNAL: &str = "runtime/state/path-update.json";

#[derive(Serialize, Deserialize)]
struct Location {
    schema: u32,
    root: PathBuf,
}

pub struct Report {
    pub moved: bool,
    pub updated_files: usize,
}
impl Report {
    pub fn message(&self) -> String {
        if self.moved {
            format!(
                "路径检查完成，已更新 {} 个文件；服务待启动验证",
                self.updated_files
            )
        } else {
            "路径检查完成".into()
        }
    }
}

fn read_optional(layout: &Layout, name: impl AsRef<Path>) -> Result<Option<Vec<u8>>> {
    let path = layout.path(name)?;
    match fs::metadata(&path) {
        Ok(meta) => ensure!(
            meta.len() <= 32 * 1024 * 1024,
            "路径检查文件过大：{}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    Ok(Some(fs::read(path)?))
}

fn native_files(layout: &Layout) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut add = |file: PathBuf| -> Result<()> {
        let file = layout.path(file)?;
        let baseline = layout.path(file.with_extension("managed.json"))?;
        if file.is_file() && baseline.is_file() {
            files.push(layout.relative(file)?);
            files.push(layout.relative(baseline)?);
        }
        ensure!(files.len() <= 1024, "配置文件数量超过检查上限");
        Ok(())
    };
    add(layout.path("config/services/apache/httpd.conf")?)?;
    for (kind, directory) in crate::discovery::configuration_directories(layout)? {
        match kind {
            Kind::Php => add(directory.join("php.ini"))?,
            Kind::Redis => add(directory.join("redis.conf"))?,
            Kind::Mysql => {
                let instances = layout.path(directory.join("instances"))?;
                if instances.is_dir() {
                    for entry in fs::read_dir(instances)? {
                        let entry = entry?;
                        if entry.file_type()?.is_dir()
                            && valid_id(&entry.file_name().to_string_lossy()).is_ok()
                        {
                            add(layout.path(entry.path())?.join("my.ini"))?;
                        }
                    }
                }
            }
            Kind::Apache => {}
        }
    }
    let apache_seed = layout.path("config/overrides/apache.conf")?;
    if apache_seed.is_file() {
        files.push(layout.relative(apache_seed)?);
    }
    Ok(files)
}

pub(crate) fn is_config_path(path: &Path) -> bool {
    native_kind(path).is_some() || path == Path::new("config/overrides/apache.conf")
}

fn native_kind(path: &Path) -> Option<Kind> {
    let parts: Vec<_> = path.iter().map(|s| s.to_str()).collect::<Option<_>>()?;
    if parts.iter().any(|p| *p == ".." || *p == ".") {
        return None;
    }
    match parts.as_slice() {
        [
            "config",
            "services",
            "apache",
            "httpd.conf" | "httpd.managed.json",
        ] => Some(Kind::Apache),
        ["bin", "php", package, rest @ ..]
            if !package.is_empty()
                && rest.len() <= 4
                && matches!(rest.last(), Some(&"php.ini" | &"php.managed.json")) =>
        {
            Some(Kind::Php)
        }
        ["bin", "redis", package, rest @ ..]
            if !package.is_empty()
                && rest.len() <= 4
                && matches!(rest.last(), Some(&"redis.conf" | &"redis.managed.json")) =>
        {
            Some(Kind::Redis)
        }
        ["bin", "mysql", package, rest @ ..]
            if !package.is_empty()
                && rest.len() >= 3
                && rest.len() <= 5
                && rest[rest.len() - 3] == "instances"
                && valid_id(rest[rest.len() - 2]).is_ok()
                && matches!(rest.last(), Some(&"my.ini" | &"my.managed.json")) =>
        {
            Some(Kind::Mysql)
        }
        _ => None,
    }
}

fn add_root(roots: &mut BTreeSet<String>, root: &Path) -> Result<()> {
    let root = local_path(root, root)?;
    ensure!(
        root.is_absolute() && root.components().count() > 2,
        "旧环境根目录无效"
    );
    roots.insert(root.to_string_lossy().replace('/', "\\").to_lowercase());
    Ok(())
}

fn previous_root(
    layout: &Layout,
    state: &Option<serde_json::Value>,
    files: &[PathBuf],
) -> Result<PathBuf> {
    if let Some(bytes) = read_optional(layout, LOCATION)? {
        let location: Location = serde_json::from_slice(&bytes).context("路径记录损坏")?;
        ensure!(location.schema == 1, "不支持的路径记录版本");
        let mut checked = BTreeSet::new();
        add_root(&mut checked, &location.root)?;
        return local_path(&location.root, &location.root);
    }
    let mut roots = BTreeSet::new();
    if let Some(entries) = state.as_ref().and_then(|s| s["applied_files"].as_object()) {
        for name in entries.keys() {
            let path = Path::new(name);
            if !path.is_absolute() {
                continue;
            }
            let parts: Vec<_> = path.components().collect();
            for i in 2..parts.len().saturating_sub(1) {
                if parts[i]
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("config")
                    && (parts[i + 1]
                        .as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("services")
                        || parts[i + 1]
                            .as_os_str()
                            .to_string_lossy()
                            .eq_ignore_ascii_case("overrides"))
                {
                    add_root(&mut roots, &parts[..i].iter().collect::<PathBuf>())?;
                    break;
                }
            }
        }
    }
    // These managed fields have fixed suffixes; user site paths do not identify an environment.
    for name in files
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".managed.json"))
    {
        let value: serde_json::Value =
            serde_json::from_slice(&read_optional(layout, name)?.context("配置基线缺失")?)?;
        for line in value["head"].as_str().unwrap_or_default().lines() {
            let suffix = if line.starts_with("sys_temp_dir=") || line.starts_with("tmpdir=") {
                "/runtime/temp"
            } else if line.starts_with("PidFile ") {
                "/runtime/state/apache.pid"
            } else if line.starts_with("dir ") {
                "/data/redis"
            } else {
                continue;
            };
            let normalized = line.replace('\\', "/");
            if let Some((_, value)) = normalized.split_once('"')
                && let Some(root) = value.trim_end_matches('"').strip_suffix(suffix)
            {
                add_root(&mut roots, Path::new(root))?;
            }
        }
    }
    ensure!(
        roots.len() <= 1,
        "配置包含多个旧环境根目录，无法自动确定路径归属"
    );
    Ok(roots
        .into_iter()
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| layout.root.clone()))
}

/// Replace complete path prefixes, respecting separators and token boundaries (including Unicode).
pub fn rebase_text(text: &str, old: &Path, new: &Path) -> String {
    let prefix = old.to_string_lossy().replace('\\', "/");
    let replacement = new.to_string_lossy().replace('\\', "/");
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    while position < text.len() {
        let rest = &text[position..];
        let start_ok = position == 0
            || text[..position].chars().next_back().is_some_and(|c| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '=' | ';' | '(' | '[')
            });
        // Compare by characters so case conversion does not corrupt UTF-8 byte offsets.
        let mut consumed = 0;
        let matched = start_ok && {
            let mut chars = rest.char_indices();
            prefix.chars().all(|expected| {
                if let Some((offset, actual)) = chars.next() {
                    consumed = offset + actual.len_utf8();
                    actual == expected
                        || (actual == '\\' && expected == '/')
                        || actual.to_lowercase().eq(expected.to_lowercase())
                } else {
                    false
                }
            })
        };
        let end_ok = matched
            && rest[consumed..]
                .chars()
                .next()
                .is_none_or(|c| matches!(c, '/' | '\\' | '"' | '\'' | ';' | '>' | ')' | ']'));
        if end_ok {
            let backslash = rest[..consumed].contains('\\');
            output.push_str(&if backslash {
                replacement.replace('/', "\\")
            } else {
                replacement.clone()
            });
            position += consumed;
        } else {
            let Some(c) = rest.chars().next() else { break };
            output.push(c);
            position += c.len_utf8();
        }
    }
    output
}

fn relocate_json_strings(value: &mut serde_json::Value, old: &Path, new: &Path) {
    match value {
        serde_json::Value::String(text) => *text = rebase_text(text, old, new),
        serde_json::Value::Array(values) => {
            for value in values {
                relocate_json_strings(value, old, new);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                relocate_json_strings(value, old, new);
            }
        }
        _ => (),
    }
}

fn validate_stopped(layout: &Layout, state: &Option<serde_json::Value>, old: &Path) -> Result<()> {
    if let Some(state) = state {
        for record in state["records"].as_array().context("运行记录格式无效")? {
            let mut identity: Identity = serde_json::from_value(record["identity"].clone())?;
            let recorded = local_path(old, &identity.executable)?;
            let relative = relative_to(&recorded, old).context("运行记录的程序不属于原环境")?;
            // Validate the destination boundary before examining this recorded PID. Never open old files.
            layout.path(relative)?;
            identity.executable = recorded;
            if let Ok(process) = crate::platform::Process::open(identity.pid) {
                ensure!(
                    !(process.running() && process.identity.created == identity.created),
                    "路径已变化，但记录的服务进程 {} 尚未退出；请先正常停止服务再重试",
                    identity.pid
                );
            } else {
                ensure!(
                    !crate::platform::recorded_process_exists(identity.pid)?,
                    "无法核验记录的服务进程 {}，暂不更新路径",
                    identity.pid
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}

fn allowed(path: &Path) -> bool {
    !path.is_absolute()
        && (path == Path::new(LOCATION)
            || path == Path::new("config/app.json")
            || path == Path::new(STATE)
            || path == Path::new("runtime/state/startup-registration.json")
            || path.starts_with("config/services")
            || path.starts_with("config/overrides")
            || is_config_path(path))
}

fn restore(layout: &Layout, changes: &[Change]) -> Result<()> {
    for change in changes {
        ensure!(allowed(&change.path), "路径恢复记录包含不支持的文件");
        let current = read_optional(layout, &change.path)?;
        ensure!(
            current == change.before || current.as_ref() == Some(&change.after),
            "配置已被其他操作修改，保留当前文件并停止自动恢复：{}",
            change.path.display()
        );
    }
    for change in changes.iter().rev() {
        ensure!(allowed(&change.path), "路径恢复记录包含不支持的文件");
        let path = layout.path(&change.path)?;
        if read_optional(layout, &change.path)? == change.before {
            continue;
        }
        if let Some(bytes) = &change.before {
            atomic_write_if_changed(&path, bytes)?;
        } else if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn commit(layout: &Layout, changes: &[Change]) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let journal = layout.path(JOURNAL)?;
    let bytes = crate::platform::secrets::seal(&serde_json::to_vec(changes)?)?;
    ensure!(bytes.len() <= 32 * 1024 * 1024, "路径更新记录超过大小上限");
    let backup = layout.path(format!(
        "runtime/path-backups/{}/changes.json",
        crate::platform::random_token()?
    ))?;
    fs::create_dir_all(backup.parent().context("路径备份目录缺失")?)?;
    atomic_write(&backup, &bytes)?;
    atomic_write(&journal, &bytes)?;
    let apply = (|| -> Result<()> {
        for change in changes {
            ensure!(allowed(&change.path), "不支持的路径更新文件");
            let path = layout.path(&change.path)?;
            ensure!(
                read_optional(layout, &change.path)? == change.before,
                "检查期间配置已变化，请重试"
            );
            atomic_write(&path, &change.after)?;
        }
        Ok(())
    })();
    if let Err(error) = apply {
        return match restore(layout, changes) {
            Ok(()) => {
                fs::remove_file(journal)?;
                Err(error.context("路径更新失败，原文件已恢复"))
            }
            Err(restore) => Err(error.context(format!(
                "路径更新失败，恢复未完成：{restore:#}；备份：{}",
                backup.display()
            ))),
        };
    }
    fs::remove_file(journal)?;
    Ok(())
}

fn stage(
    layout: &Layout,
    changes: &mut Vec<Change>,
    path: impl AsRef<Path>,
    after: Vec<u8>,
) -> Result<()> {
    let path = path.as_ref();
    let before = read_optional(layout, path)?;
    if before.as_ref() != Some(&after) {
        changes.push(Change {
            path: path.to_owned(),
            before,
            after,
        });
    }
    Ok(())
}

pub fn check(layout: &Layout) -> Result<Report> {
    if let Some(bytes) = read_optional(layout, JOURNAL)? {
        let changes: Vec<Change> = serde_json::from_slice(&crate::platform::secrets::open(&bytes)?)
            .context("路径更新恢复记录损坏")?;
        for change in &changes {
            ensure!(allowed(&change.path), "路径恢复记录越界");
            layout.path(&change.path)?;
        }
        restore(layout, &changes).context("未完成的路径更新无法恢复")?;
        fs::remove_file(layout.path(JOURNAL)?)?;
    }
    let files = native_files(layout)?;
    let mut state: Option<serde_json::Value> = read_optional(layout, STATE)?
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()
        .context("运行记录损坏，无法检查路径")?;
    let old = previous_root(layout, &state, &files)?;
    let moved = !path_within(&old, &layout.root) || !path_within(&layout.root, &old);
    let mut changes = Vec::new();
    if moved {
        validate_stopped(layout, &state, &old)?;
    }
    if let Some(bytes) = read_optional(layout, "config/app.json")? {
        let mut config: Config = serde_json::from_slice(&bytes).context("无法读取 app.json")?;
        let original = serde_json::to_value(&config)?;
        for value in config.projects.iter_mut().map(|p| &mut p.directory).chain(
            config
                .php
                .iter_mut()
                .map(|p| &mut p.directory_entry.directory),
        ) {
            if value.is_absolute() {
                let normalized = local_path(&old, value)?;
                if let Some(relative) = relative_to(&normalized, &old) {
                    *value = relative;
                }
            }
        }
        config.relativize_paths(layout)?;
        config.validate(layout)?;
        // Preserve formatting when semantic paths did not change.
        if serde_json::to_value(&config)? != original {
            stage(
                layout,
                &mut changes,
                "config/app.json",
                serde_json::to_vec_pretty(&config)?,
            )?;
        }
    }
    if moved {
        for path in &files {
            let bytes = read_optional(layout, path)?.context("配置文件缺失")?;
            let text = std::str::from_utf8(&bytes).context("配置文件必须使用 UTF-8")?;
            let next = if path.extension().is_some_and(|e| e == "json") {
                let mut value: serde_json::Value = serde_json::from_str(text)?;
                let original = value.clone();
                relocate_json_strings(&mut value, &old, &layout.root);
                if value == original {
                    bytes.clone()
                } else {
                    serde_json::to_vec_pretty(&value)?
                }
            } else if native_kind(path) == Some(Kind::Redis)
                || path == Path::new("config/overrides/redis.conf")
            {
                text.split_inclusive('\n')
                    .map(|line| {
                        if crate::redis_config::directive(line).eq_ignore_ascii_case("requirepass")
                        {
                            line.to_owned()
                        } else {
                            rebase_text(line, &old, &layout.root)
                        }
                    })
                    .collect::<String>()
                    .into_bytes()
            } else {
                rebase_text(text, &old, &layout.root).into_bytes()
            };
            stage(layout, &mut changes, path, next)?;
        }
        for path in files.iter().filter(|p| {
            native_kind(p).is_some() && p.extension().is_some_and(|e| e == "ini" || e == "conf")
        }) {
            let kind = native_kind(path).context("未知实例配置目录")?;
            let candidate = |name: &Path| -> Result<Vec<u8>> {
                if let Some(change) = changes.iter().find(|change| change.path == name) {
                    Ok(change.after.clone())
                } else {
                    read_optional(layout, name)?.context("实例配置或基线缺失")
                }
            };
            crate::service_config::validate_relocated(
                layout,
                kind,
                &candidate(path)?,
                &candidate(&path.with_extension("managed.json"))?,
            )
            .with_context(|| format!("路径更新后的配置校验失败：{}", path.display()))?;
        }
        // Old runtime snapshots are not evidence that anything is applied at the new location.
        if let Some(state) = state.as_mut() {
            state["records"] = serde_json::json!([]);
            state["applied"] = serde_json::Value::Null;
            state["applied_files"] = serde_json::json!({});
            stage(
                layout,
                &mut changes,
                STATE,
                serde_json::to_vec_pretty(state)?,
            )?;
        }
        if layout.development_root().is_some()
            && let Some(bytes) = read_optional(layout, "runtime/state/startup-registration.json")?
        {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
            relocate_json_strings(&mut value, &old, &layout.root);
            stage(
                layout,
                &mut changes,
                "runtime/state/startup-registration.json",
                serde_json::to_vec(&value)?,
            )?;
        }
    }
    let location = Location {
        schema: 1,
        root: layout.root.clone(),
    };
    // The location is committed last; failed/interrupted transactions restore all prior files.
    if moved || read_optional(layout, LOCATION)?.is_none() {
        stage(
            layout,
            &mut changes,
            LOCATION,
            serde_json::to_vec_pretty(&location)?,
        )?;
    }
    let updated_files = changes.len();
    commit(layout, &changes)?;
    Ok(Report {
        moved,
        updated_files,
    })
}
