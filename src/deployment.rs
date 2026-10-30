//! Directory deployments own only the files explicitly listed in a release manifest.
use crate::{
    Context, Result, bail,
    config::{local_path, reject_links},
    ensure, platform,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAGIC: &[u8; 16] = b"FLARAGONPACK0001";
const LIMIT: u64 = 512 * 1024 * 1024;
const RELEASE: &str = "release.json";
const REPAIR_BASE: &str = "repair-base.json";
const UPDATE_BASE: &str = "update-base.json";
const INSTALLATION: &str = ".flaragon-install.json";
const ACTIVE: &str = ".maintenance/active.json";
const OWNER: &str = ".maintenance/owner.json";
const OWNER_BYTES: &[u8] = b"{\"product\":\"Flaragon\",\"format\":1}";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Mode {
    Install,
    Update,
    Repair,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub product: String,
    pub version: String,
    pub release_id: String,
    pub architecture: String,
    pub format: u32,
    pub config_schema: u32,
    pub minimum_version: String,
    pub files: BTreeMap<String, u64>,
    #[serde(default)]
    pub remove: BTreeSet<String>,
    #[serde(default)]
    pub excluded_versions: BTreeSet<String>,
}

pub struct Package {
    pub mode: Mode,
    pub manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
    repair_base: Option<Manifest>,
    update_base: Option<Manifest>,
}

fn version(value: &str) -> Result<[u16; 3]> {
    let parts = value.split('.').collect::<Vec<_>>();
    ensure!(parts.len() == 3, "无效的软件版本：{value}");
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty()
                && (part.len() == 1 || !part.starts_with('0'))
                && part.bytes().all(|c| c.is_ascii_digit()),
            "无效的软件版本"
        );
        result[index] = part.parse()?;
    }
    Ok(result)
}

fn program_file(name: &str) -> Result<()> {
    ensure!(
        !name.contains('\\') && !name.starts_with('/') && name.len() <= 240,
        "发布文件路径无效：{name}"
    );
    ensure!(
        name == "Flaragon.exe"
            || name == "lib/Scintilla.dll"
            || name == "lib/Lexilla.dll"
            || name.starts_with("licenses/"),
        "发布清单包含非程序文件：{name}"
    );
    ensure!(
        name.split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".."),
        "发布文件路径越界"
    );
    local_path(Path::new(r"C:\Flaragon"), Path::new(name))?;
    Ok(())
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.product == "Flaragon"
                && self.architecture == "x86_64-pc-windows-msvc"
                && self.format == 1
                && self.config_schema == 1,
            "不支持的产品、架构或发布格式"
        );
        ensure!(
            version(&self.minimum_version)? <= version(&self.version)?,
            "允许升级的版本范围无效"
        );
        ensure!(valid_token(&self.release_id), "发布标识无效");
        for excluded in &self.excluded_versions {
            version(excluded)?;
        }
        ensure!(
            !self.files.is_empty() && self.files.len() <= 8192,
            "发布文件数量异常"
        );
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for (name, length) in &self.files {
            program_file(name)?;
            ensure!(seen.insert(name.to_lowercase()), "发布文件名称冲突");
            total = total.checked_add(*length).context("发布文件过大")?;
            ensure!(total <= LIMIT, "发布文件超过大小限制");
        }
        for required in ["Flaragon.exe", "lib/Scintilla.dll", "lib/Lexilla.dll"] {
            ensure!(
                self.files.get(required).is_some_and(|n| *n > 0),
                "发布包缺少 {required}"
            );
        }
        for name in self.files.keys() {
            let mut parent = Path::new(name).parent();
            while let Some(path) = parent {
                ensure!(
                    !seen.contains(&path.to_string_lossy().replace('\\', "/").to_lowercase()),
                    "发布清单中文件与目录冲突"
                );
                parent = path.parent();
            }
        }
        for name in &self.remove {
            program_file(name)?;
            ensure!(seen.insert(name.to_lowercase()), "删除清单与程序文件冲突");
        }
        ensure!(
            self.files.keys().any(|s| s.starts_with("licenses/")),
            "发布包缺少许可文件"
        );
        Ok(())
    }
}

impl Package {
    pub fn open(executable: &Path) -> Result<Self> {
        reject_links(executable)?;
        let mut file = File::open(executable)?;
        let size = file.metadata()?.len();
        ensure!(size > 25, "此文件不是安装或升级包");
        file.seek(SeekFrom::End(-25))?;
        let mut trailer = [0u8; 25];
        file.read_exact(&mut trailer)?;
        ensure!(
            &trailer[9..] == MAGIC,
            "缺少安装包载荷，请使用正式发布的安装或升级 EXE"
        );
        let length = u64::from_le_bytes(trailer[..8].try_into()?);
        ensure!(
            length > 0 && length <= LIMIT && length < size - 25,
            "载荷长度无效"
        );
        let mode = match trailer[8] {
            0 => Mode::Install,
            1 => Mode::Update,
            2 => Mode::Repair,
            _ => bail!("载荷模式无效"),
        };
        file.seek(SeekFrom::Start(size - 25 - length))?;
        let mut decoded = Vec::new();
        // Reading through the gzip trailer validates its built-in CRC, before any target is changed.
        flate2::read::GzDecoder::new(file.take(length))
            .take(LIMIT + 1)
            .read_to_end(&mut decoded)
            .context("压缩包损坏")?;
        ensure!(decoded.len() as u64 <= LIMIT, "解压内容超过大小限制");
        Self::from_archive(mode, &decoded)
    }

    fn from_archive(mode: Mode, bytes: &[u8]) -> Result<Self> {
        let mut files = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut archive = tar::Archive::new(bytes);
        for entry in archive.entries()? {
            let mut entry = entry?;
            ensure!(
                entry.header().entry_type().is_file(),
                "安装包只允许普通文件"
            );
            let name = entry
                .path()?
                .to_str()
                .context("安装包路径编码无效")?
                .to_owned();
            if name != RELEASE
                && !(mode == Mode::Repair && name == REPAIR_BASE)
                && !(mode == Mode::Update && name == UPDATE_BASE)
            {
                program_file(&name)?;
            }
            ensure!(
                seen.insert(name.to_lowercase()) && seen.len() <= 8194,
                "安装包存在重复路径或文件过多"
            );
            ensure!(entry.size() <= LIMIT, "安装文件过大");
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            files.insert(name, contents);
        }
        let manifest: Manifest =
            serde_json::from_slice(files.get(RELEASE).context("安装包缺少发布清单")?)?;
        manifest.validate()?;
        let repair_base = if mode == Mode::Repair {
            let base: Manifest = serde_json::from_slice(
                &files
                    .remove(REPAIR_BASE)
                    .context("修复包缺少适用安装清单")?,
            )?;
            base.validate()?;
            repair_allowed(&base, &manifest)?;
            Some(base)
        } else {
            None
        };
        let update_base = files
            .remove(UPDATE_BASE)
            .map(|bytes| -> Result<Manifest> {
                let base: Manifest = serde_json::from_slice(&bytes)?;
                base.validate()?;
                repair_allowed(&base, &manifest)?;
                Ok(base)
            })
            .transpose()?;
        ensure!(
            files.len() == manifest.files.len() + 1,
            "安装包与发布清单不一致"
        );
        for (name, length) in &manifest.files {
            ensure!(
                files.get(name).is_some_and(|v| v.len() as u64 == *length),
                "安装文件缺失或长度不一致：{name}"
            );
        }
        Ok(Self {
            mode,
            manifest,
            files,
            repair_base,
            update_base,
        })
    }

    fn update_allowed(&self, old: &Manifest) -> Result<()> {
        if old.version == self.manifest.version {
            ensure!(old != &self.manifest, "此安装已是该构建，无需重复更新");
            ensure!(
                self.update_base.as_ref() == Some(old),
                "此安装不属于覆盖升级包指定的原始构建"
            );
            repair_allowed(old, &self.manifest)
        } else {
            upgrade_allowed(old, &self.manifest)
        }
    }
}

fn inside(path: &Path, base: &Path) -> bool {
    let path = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let base = base
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    path == base || path.starts_with(&(base + "\\"))
}

/// Validates without creating directories or loading application settings.
pub fn target_root(value: &Path) -> Result<PathBuf> {
    ensure!(value.is_absolute(), "请选择完整的本机安装目录");
    let mut root = local_path(value, value)?;
    crate::config::validate_directory_path(&root)?;
    if let Some(boundary) = std::env::var_os("FLARAGON_DEV_ROOT") {
        let boundary = PathBuf::from(boundary);
        ensure!(
            boundary.is_absolute() && inside(&root, &boundary),
            "目标目录超出允许范围"
        );
    }
    ensure!(root.components().count() > 2, "不能使用盘符根目录");
    for variable in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(path) = std::env::var_os(variable) {
            ensure!(
                !inside(&root, &PathBuf::from(path)),
                "请选择当前用户可写的独立目录"
            );
        }
    }
    reject_links(&root)?;
    if root.exists() {
        let canonical = fs::canonicalize(&root)?;
        root = PathBuf::from(canonical.to_string_lossy().trim_start_matches(r"\\?\"));
        if let Some(boundary) = std::env::var_os("FLARAGON_DEV_ROOT") {
            ensure!(
                inside(&root, &PathBuf::from(boundary)),
                "目标目录超出允许范围"
            );
        }
    }
    ensure!(
        !root.join("Cargo.toml").exists() && !root.join(".git").exists(),
        "不能安装或更新源码目录，请选择独立使用目录"
    );
    if root.exists() {
        ensure!(root.is_dir(), "目标不是目录");
    }
    Ok(root)
}

fn checked(root: &Path, name: &str) -> Result<PathBuf> {
    let path = local_path(root, Path::new(name))?;
    ensure!(inside(&path, root), "部署路径越界");
    reject_links(&path)?;
    Ok(path)
}

pub fn lock_directory(root: &Path) -> Result<File> {
    let path = checked(root, ".flaragon.lock")?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .context("Flaragon 尚未退出或目录正被占用。请先“停止并退出”后重试，已终止更新")
}

pub fn ensure_ready(root: &Path) -> Result<()> {
    ensure!(
        !checked(root, ACTIVE)?.exists(),
        "安装或升级尚未完成，请重新运行同一目标版本的安装或升级包完成修复"
    );
    Ok(())
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>> {
    reject_links(path)?;
    let mut result = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut result)?;
    ensure!(
        result.len() as u64 <= max,
        "文件超过读取限制：{}",
        path.display()
    );
    Ok(result)
}

fn installed(root: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(
        &read_limited(&checked(root, RELEASE)?, 4 * 1024 * 1024)
            .context("未找到正式安装的发布清单，不能升级此目录")?,
    )?;
    manifest.validate()?;
    Ok(manifest)
}

fn configuration_check(root: &Path) -> Result<()> {
    let path = checked(root, "config/app.json")?;
    if path.exists() {
        let config: crate::config::Config =
            serde_json::from_slice(&read_limited(&path, 16 * 1024 * 1024)?)?;
        ensure!(config.schema == 1, "配置格式不兼容，已终止更新");
    }
    let path = checked(root, "config/startup.json")?;
    if path.exists() {
        let _: crate::startup::Settings =
            serde_json::from_slice(&read_limited(&path, 1024 * 1024)?)?;
    }
    Ok(())
}

fn stopped(root: &Path) -> Result<()> {
    #[derive(Deserialize)]
    struct State {
        records: Vec<Record>,
    }
    #[derive(Deserialize)]
    struct Record {
        identity: platform::Identity,
    }
    let path = checked(root, "runtime/state/processes.json")?;
    if !path.exists() {
        return Ok(());
    }
    let state: State = serde_json::from_slice(&read_limited(&path, 128 * 1024 * 1024)?)
        .context("运行记录无法读取，不能确认服务已退出")?;
    for mut record in state.records {
        let executable = checked(
            root,
            record
                .identity
                .executable
                .to_str()
                .context("进程路径无效")?,
        )?;
        ensure!(
            inside(&executable, &root.join("bin")),
            "服务记录不属于此环境，已终止更新"
        );
        record.identity.executable = executable;
        ensure!(
            !platform::deployment::recorded_process_running(&record.identity)?,
            "受管服务尚未退出。请打开 Flaragon 执行“停止并退出”，已终止更新"
        );
    }
    Ok(())
}

fn maintain(root: &Path) -> Result<()> {
    let owner = checked(root, OWNER)?;
    if owner.exists() {
        ensure!(
            read_limited(&owner, 1024)? == OWNER_BYTES,
            "维护目录归属不明"
        );
    } else {
        let directory = checked(root, ".maintenance")?;
        ensure!(!directory.exists(), "维护目录已被其他内容占用");
        fs::create_dir(&directory)?;
        write_program(root, &owner, OWNER_BYTES)?;
    }
    Ok(())
}

fn write_program(root: &Path, destination: &Path, bytes: &[u8]) -> Result<()> {
    reject_links(destination)?;
    let temp = checked(
        root,
        &format!(".maintenance/write-{}.tmp", platform::random_token()?),
    )?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let result = platform::replace_file(&temp, destination);
    if result.is_err() && temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn valid_token(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Installation {
    product: String,
    format: u32,
    id: String,
}
impl Installation {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.product == "Flaragon" && self.format == 1 && valid_token(&self.id),
            "不是有效的 Flaragon 安装身份"
        );
        Ok(())
    }
}
fn installation(root: &Path) -> Result<Installation> {
    let value: Installation = serde_json::from_slice(
        &read_limited(&checked(root, INSTALLATION)?, 4096)
            .context("不是正式的 Flaragon 软件目录：缺少安装身份文件")?,
    )?;
    value.validate()?;
    Ok(value)
}
fn install_directory_empty(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let owner = checked(root, OWNER)?;
    let owned = owner.exists() && read_limited(&owner, 1024)? == OWNER_BYTES;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = checked(root, entry.file_name().to_str().context("目录名称无效")?)?;
        if entry.file_name() == ".flaragon.lock" && path.is_file() && fs::metadata(path)?.len() == 0
        {
            continue;
        }
        if owned && entry.file_name() == ".maintenance" {
            continue;
        }
        bail!("安装目录非空，请选择新目录；已有正式安装请使用升级包");
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: u32,
    token: String,
    mode: Mode,
    installation: Installation,
    previous: Option<Manifest>,
    next: Manifest,
}
impl Journal {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.format == 1 && valid_token(&self.token),
            "未完成更新记录无效"
        );
        self.installation.validate()?;
        self.next.validate()?;
        if let Some(old) = &self.previous {
            old.validate()?;
            if self.mode == Mode::Repair
                || (self.mode == Mode::Update && old.version == self.next.version)
            {
                repair_allowed(old, &self.next)?;
            } else {
                upgrade_allowed(old, &self.next)?;
            }
        }
        ensure!(
            (self.mode != Mode::Install) == self.previous.is_some(),
            "更新记录模式无效"
        );
        Ok(())
    }
    fn folder(&self) -> String {
        format!(".maintenance/{}", self.token)
    }
    fn names(&self) -> BTreeSet<String> {
        let mut result = self.next.files.keys().cloned().collect::<BTreeSet<_>>();
        if let Some(old) = &self.previous {
            result.extend(old.files.keys().cloned());
        }
        result.insert(RELEASE.into());
        result.insert(INSTALLATION.into());
        result
    }
}
fn pending(root: &Path, package: &Package) -> Result<Option<Journal>> {
    let path = checked(root, ACTIVE)?;
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        read_limited(&checked(root, OWNER)?, 1024)? == OWNER_BYTES,
        "维护目录归属不明"
    );
    let journal: Journal = serde_json::from_slice(&read_limited(&path, 8 * 1024 * 1024)?)?;
    journal.validate()?;
    if package.mode == Mode::Repair {
        ensure!(
            package.repair_base.is_some() && journal.previous == package.repair_base,
            "未完成修复与此包的适用安装不符"
        );
    }
    if package.mode == Mode::Update {
        package.update_allowed(journal.previous.as_ref().context("更新记录缺少原始安装")?)?;
    }
    ensure!(
        journal.next == package.manifest && journal.mode == package.mode,
        "目录存在未完成更新，请使用原目标版本 {} 的同一发布包修复；不支持回退",
        journal.next.version
    );
    if checked(root, INSTALLATION)?.exists() {
        ensure!(
            installation(root)? == journal.installation,
            "安装身份与未完成记录不符"
        );
    } else {
        ensure!(journal.mode == Mode::Install, "安装身份缺失，无法修复");
    }
    if let Some(old) = &journal.previous {
        let current = installed(root)?;
        ensure!(
            current == *old || current == journal.next,
            "发布清单与未完成记录不符"
        );
    }
    // A resumed package must match the complete staged contents, not just a version label.
    for (name, bytes) in &package.files {
        ensure!(
            read_limited(
                &checked(root, &format!("{}/new/{name}", journal.folder()))?,
                LIMIT
            )? == *bytes,
            "暂存内容与此发布包不符：{name}；不能继续更新"
        );
    }
    Ok(Some(journal))
}
fn upgrade_allowed(old: &Manifest, next: &Manifest) -> Result<()> {
    ensure!(
        version(&next.version)? > version(&old.version)?,
        "当前版本相同或更高，已终止更新；不支持降级"
    );
    if version(&next.version)? >= [0, 0, 4] {
        ensure!(
            old.version == next.minimum_version,
            "此升级包仅支持从上一正式版本 {} 升级，当前为 {}；请按正式版本顺序升级",
            next.minimum_version,
            old.version
        );
    }
    ensure!(
        version(&old.version)? >= version(&next.minimum_version)?
            && !next.excluded_versions.contains(&old.version),
        "当前版本不在此包支持的升级范围内"
    );
    ensure!(
        old.config_schema == next.config_schema,
        "配置格式不兼容，已终止更新"
    );
    ensure!(
        old.files
            .keys()
            .all(|n| next.files.contains_key(n) || next.remove.contains(n)),
        "升级包缺少旧程序文件，需要明确的文件移除声明"
    );
    Ok(())
}
fn repair_allowed(old: &Manifest, next: &Manifest) -> Result<()> {
    ensure!(
        old.version == next.version && old.release_id != next.release_id,
        "修复包仅适用于指定版本的未修复安装"
    );
    ensure!(
        old.product == next.product
            && old.architecture == next.architecture
            && old.format == next.format
            && old.config_schema == next.config_schema
            && old.minimum_version == next.minimum_version
            && old.excluded_versions == next.excluded_versions
            && next.remove.is_empty()
            && old.files.keys().eq(next.files.keys())
            && old.files.iter().all(|(name, size)| {
                name == "Flaragon.exe" || next.files.get(name) == Some(size)
            }),
        "修复包必须保持产品版本、配置格式和程序文件集合"
    );
    Ok(())
}
fn installed_files(
    root: &Path,
    manifest: &Manifest,
    identify: fn(&Path, &str) -> Result<()>,
) -> Result<()> {
    for (name, length) in &manifest.files {
        let meta =
            fs::metadata(checked(root, name)?).with_context(|| format!("软件文件缺失：{name}"))?;
        ensure!(
            meta.is_file() && meta.len() == *length,
            "程序文件与发布清单不符：{name}"
        );
    }
    identify(&checked(root, "Flaragon.exe")?, &manifest.version)
}

/// Carries exactly the directory and release the user reviewed. Recomputed under the deployment lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    root: PathBuf,
    directory: Option<[u32; 3]>,
    installation: Option<Installation>,
    previous: Option<Manifest>,
    pending: Option<Journal>,
}
impl Review {
    pub fn description(&self, package: &Package) -> String {
        let current = self
            .previous
            .as_ref()
            .map(|m| m.version.as_str())
            .unwrap_or("未安装");
        format!(
            "软件位置：{}\n当前版本：{}\n目标版本：{}\n{}\n仅更新软件本体，保留组件、配置、项目和数据库。",
            self.root.display(),
            current,
            package.manifest.version,
            if self.pending.is_some() {
                "此目录存在未完成操作，将继续修复目标版本。"
            } else if package.mode == Mode::Repair {
                "已确认此安装适用修复包；修复后产品版本号保持不变。"
            } else if package.mode == Mode::Update {
                if current == package.manifest.version {
                    "已确认原始构建，允许同版本覆盖升级；产品版本号保持不变。"
                } else {
                    "已确认 Flaragon 安装身份与程序文件。"
                }
            } else {
                "目录检查通过，可以安装。"
            }
        )
    }
}
fn inspect_with(
    package: &Package,
    value: &Path,
    identify: fn(&Path, &str) -> Result<()>,
) -> Result<Review> {
    package.manifest.validate()?;
    let root = target_root(value)?;
    let directory = if root.exists() {
        Some(platform::deployment::directory_guard(&root)?.1)
    } else {
        None
    };
    if let Some(journal) = pending(&root, package)? {
        return Ok(Review {
            root,
            directory,
            installation: Some(journal.installation.clone()),
            previous: journal.previous.clone(),
            pending: Some(journal),
        });
    }
    let (identity, previous) = match package.mode {
        Mode::Install => {
            install_directory_empty(&root)?;
            (None, None)
        }
        Mode::Update | Mode::Repair => {
            let identity = installation(&root)?;
            let old = installed(&root)?;
            installed_files(&root, &old, identify)?;
            // Occupied files are rejected even when the selected package happens to be the same version.
            platform::deployment::ensure_files_free(&root, old.files.keys().cloned())?;
            stopped(&root)?;
            if package.mode == Mode::Repair {
                ensure!(old != package.manifest, "此安装已应用该修复，无需重复更新");
                ensure!(
                    package.repair_base.as_ref() == Some(&old),
                    "此安装不属于修复包指定的原始版本或构建"
                );
                repair_allowed(&old, &package.manifest)?;
            } else {
                package.update_allowed(&old)?;
            }
            for name in package.manifest.files.keys() {
                ensure!(
                    !checked(&root, name)?.exists() || old.files.contains_key(name),
                    "文件不属于此安装，拒绝覆盖：{name}"
                );
            }
            (Some(identity), Some(old))
        }
    };
    configuration_check(&root)?;
    Ok(Review {
        root,
        directory,
        installation: identity,
        previous,
        pending: None,
    })
}
pub fn review_target(package: &Package, value: &Path) -> Result<Review> {
    inspect_with(package, value, platform::deployment::check_program_identity)
}
pub fn inspect_target(package: &Package, value: &Path) -> Result<String> {
    Ok(review_target(package, value)?.description(package))
}
pub fn deploy(package: &Package, value: &Path) -> Result<String> {
    let review = review_target(package, value)?;
    deploy_reviewed(package, value, &review)
}
pub fn deploy_reviewed(package: &Package, value: &Path, review: &Review) -> Result<String> {
    deploy_with(
        package,
        value,
        Some(review),
        self_check_process,
        platform::deployment::check_program_identity,
        None,
    )
}
fn deploy_with(
    package: &Package,
    value: &Path,
    reviewed: Option<&Review>,
    verify: fn(&Path, &str) -> Result<()>,
    identify: fn(&Path, &str) -> Result<()>,
    fail_after: Option<usize>,
) -> Result<String> {
    let root = target_root(value)?;
    let initial = inspect_with(package, &root, identify)?;
    if let Some(review) = reviewed {
        ensure!(*review == initial, "软件位置或版本已变化，请重新检查目录");
    }
    if !root.exists() {
        fs::create_dir_all(&root)?;
    }
    let (_guard, directory) = platform::deployment::directory_guard(&root)?;
    if let Some(expected) = initial.directory {
        ensure!(expected == directory, "目标目录身份变化，已终止更新");
    }
    let _lock = lock_directory(&root)?;
    let mut current = inspect_with(package, &root, identify)?;
    current.directory = initial.directory;
    ensure!(current == initial, "目标安装在检查后发生变化，已终止更新");
    stopped(&root)?;
    configuration_check(&root)?;
    let journal = if let Some(journal) = initial.pending {
        journal
    } else {
        Journal {
            format: 1,
            token: platform::random_token()?,
            mode: package.mode,
            installation: initial.installation.unwrap_or(Installation {
                product: "Flaragon".into(),
                format: 1,
                id: platform::random_token()?,
            }),
            previous: initial.previous,
            next: package.manifest.clone(),
        }
    };
    platform::deployment::ensure_files_free(&root, journal.names().into_iter())?;
    let payload_size = package.files.values().map(|v| v.len() as u64).sum::<u64>();
    ensure!(
        platform::deployment::free_space(&root)? >= payload_size * 3 + 16 * 1024 * 1024,
        "可用磁盘空间不足，未开始覆盖"
    );
    maintain(&root)?;
    if !checked(&root, ACTIVE)?.exists() {
        let folder = checked(&root, &journal.folder())?;
        fs::create_dir(&folder)?;
        for (name, bytes) in &package.files {
            let path = checked(&root, &format!("{}/new/{name}", journal.folder()))?;
            fs::create_dir_all(path.parent().context("暂存目录无效")?)?;
            let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        write_program(
            &root,
            &checked(&root, ACTIVE)?,
            &serde_json::to_vec_pretty(&journal)?,
        )?;
    }
    let apply = (|| -> Result<()> {
        if !checked(&root, INSTALLATION)?.exists() {
            write_program(
                &root,
                &checked(&root, INSTALLATION)?,
                &serde_json::to_vec_pretty(&journal.installation)?,
            )?;
        }
        for (index, (name, bytes)) in package
            .files
            .iter()
            .filter(|(n, _)| n.as_str() != RELEASE)
            .enumerate()
        {
            if fail_after == Some(index) {
                bail!("injected replacement interruption");
            }
            let path = checked(&root, name)?;
            fs::create_dir_all(path.parent().context("安装目录无效")?)?;
            write_program(&root, &path, bytes)?;
        }
        if let Some(old) = &journal.previous {
            for name in &journal.next.remove {
                let path = checked(&root, name)?;
                if old.files.contains_key(name) && path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
        if journal.mode == Mode::Install {
            for name in ["bin/apache", "bin/php", "bin/mysql", "bin/redis"] {
                fs::create_dir_all(checked(&root, name)?)?;
            }
        }
        verify(&root, &journal.next.version)?;
        write_program(
            &root,
            &checked(&root, RELEASE)?,
            package.files.get(RELEASE).context("发布清单缺失")?,
        )?;
        fs::remove_file(checked(&root, ACTIVE)?)?;
        Ok(())
    })();
    if let Err(error) = apply {
        bail!(
            "{error:#}；更新尚未完成，未回退旧版本。请重新运行同一发布包完成目标版本 {} 的修复。",
            journal.next.version
        );
    }
    // Only delete files created and listed by this transaction; never traverse user directories.
    let mut cleanup_failed = false;
    for name in package.files.keys() {
        let path = checked(&root, &format!("{}/new/{name}", journal.folder()))?;
        if fs::remove_file(&path).is_err() {
            cleanup_failed = true;
        }
        let mut parent = path.parent();
        while let Some(dir) = parent {
            if dir == root.join(".maintenance") {
                break;
            }
            if fs::remove_dir(dir).is_err() {
                break;
            }
            parent = dir.parent();
        }
    }
    Ok(format!(
        "{}完成：Flaragon {}\n{}{}",
        if package.mode == Mode::Install {
            "安装"
        } else if package.mode == Mode::Repair {
            "修复"
        } else {
            "升级"
        },
        package.manifest.version,
        root.display(),
        if cleanup_failed {
            "\n部分暂存文件未能清理，不影响安装结果。"
        } else {
            ""
        }
    ))
}

fn self_check_process(root: &Path, expected: &str) -> Result<()> {
    let mut command = platform::command(&checked(root, "Flaragon.exe")?, root, root);
    if let Some(boundary) = std::env::var_os("FLARAGON_DEV_ROOT") {
        command.env("FLARAGON_DEV_ROOT", boundary);
    }
    let mut child = command
        .arg("--deployment-check")
        .arg(expected)
        .arg(root)
        .spawn()
        .context("无法启动新版自检")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "新版自检失败");
            return Ok(());
        }
        if Instant::now() >= deadline {
            // Only this read-only child is stopped, never a manager or service process.
            child.kill().context("自检超时且无法退出，保留恢复记录")?;
            child.wait()?;
            bail!("新版自检超时");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn self_check(root: &Path, expected: &str) -> Result<()> {
    let root = target_root(root)?;
    ensure!(VERSION == expected, "软件版本与升级包不匹配");
    let actual = std::env::current_exe()?;
    ensure!(
        actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&root.join("Flaragon.exe").to_string_lossy()),
        "自检程序不属于目标目录"
    );
    configuration_check(&root)?;
    platform::deployment::check_editor_dll(
        &checked(&root, "lib/Scintilla.dll")?,
        b"Scintilla_DirectFunction\0",
    )?;
    platform::deployment::check_editor_dll(&checked(&root, "lib/Lexilla.dll")?, b"CreateLexer\0")?;
    Ok(())
}

pub fn open_installed(value: &Path) -> Result<()> {
    let root = target_root(value)?;
    ensure_ready(&root)?;
    let release = installed(&root)?;
    installation(&root)?;
    installed_files(
        &root,
        &release,
        platform::deployment::check_program_identity,
    )?;
    let mut command = platform::command(&checked(&root, "Flaragon.exe")?, &root, &root);
    if let Some(boundary) = std::env::var_os("FLARAGON_DEV_ROOT") {
        command.env("FLARAGON_DEV_ROOT", boundary);
    }
    command.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests;
