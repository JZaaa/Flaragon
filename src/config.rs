use crate::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Apache,
    Php,
    Mysql,
    Redis,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Apache => "Apache",
            Self::Php => "PHP",
            Self::Mysql => "MySQL",
            Self::Redis => "Redis",
        }
    }
    pub fn folder(self) -> &'static str {
        match self {
            Self::Apache => "apache",
            Self::Php => "php",
            Self::Mysql => "mysql",
            Self::Redis => "redis",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: Kind,
    pub directory: PathBuf,
    pub executable: PathBuf,
    pub version: String,
    pub architecture: String,
    pub php_thread_safe: Option<bool>,
    pub error: Option<String>,
}

impl Package {
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceUsage {
    Common,
    #[default]
    Normal,
    Disabled,
}
impl<'de> Deserialize<'de> for ServiceUsage {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Legacy(bool),
            Name(String),
        }
        match Input::deserialize(deserializer)? {
            Input::Legacy(true) => Ok(Self::Common),
            Input::Legacy(false) => Ok(Self::Normal),
            Input::Name(s) => match s.as_str() {
                "common" => Ok(Self::Common),
                "normal" => Ok(Self::Normal),
                "disabled" => Ok(Self::Disabled),
                _ => Err(serde::de::Error::custom("使用方式无效")),
            },
        }
    }
}
impl ServiceUsage {
    pub fn index(self) -> i32 {
        match self {
            Self::Common => 0,
            Self::Normal => 1,
            Self::Disabled => 2,
        }
    }
    pub fn from_index(index: i32) -> Result<Self> {
        match index {
            0 => Ok(Self::Common),
            1 => Ok(Self::Normal),
            2 => Ok(Self::Disabled),
            _ => bail!("使用方式无效"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhpSpec {
    pub package: String,
    pub port: u16,
    #[serde(default = "two")]
    pub workers: u16,
    #[serde(default)]
    pub usage: ServiceUsage,
    #[serde(default)]
    pub directory_entry: DirectoryEntry,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectoryEntry {
    pub enabled: bool,
    pub port: u16,
    pub directory: PathBuf,
    pub address: String,
}
impl Default for DirectoryEntry {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 0,
            directory: "www".into(),
            address: loopback(),
        }
    }
}
fn loopback() -> String {
    "127.0.0.1".into()
}
pub fn listen_addresses(value: &str) -> Result<Vec<std::net::Ipv4Addr>> {
    use std::net::Ipv4Addr;
    let ip: Ipv4Addr = value.parse().context("监听地址必须是 IPv4 地址")?;
    ensure!(
        !ip.is_multicast() && !ip.is_broadcast() && (ip.is_unspecified() || ip.octets()[0] != 0),
        "监听地址无效"
    );
    Ok(if ip.is_unspecified() || ip == Ipv4Addr::LOCALHOST {
        vec![ip]
    } else {
        vec![Ipv4Addr::LOCALHOST, ip]
    })
}
fn two() -> u16 {
    2
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MysqlSpec {
    pub id: String,
    pub package: String,
    pub port: u16,
    #[serde(default, alias = "enabled")]
    pub usage: ServiceUsage,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub series: String,
    #[serde(default)]
    pub reuse_data: bool,
}
impl MysqlSpec {
    pub fn data_path(&self, layout: &Layout) -> Result<PathBuf> {
        ensure!(
            valid_mysql_series(&self.series),
            "MySQL 版本目录无效，请选择有效组件并重新创建旧格式服务"
        );
        valid_id(&self.name)?;
        layout.path(format!("data/mysql/{}/{}", self.series, self.name))
    }
    pub fn binding_path(&self, layout: &Layout) -> Result<PathBuf> {
        layout.path(self.data_path(layout)?.join(".flaragon-binding.json"))
    }
}
pub fn mysql_series(version: &str) -> Result<String> {
    let series = version.split('.').take(2).collect::<Vec<_>>().join(".");
    ensure!(
        valid_mysql_series(&series),
        "无法识别 MySQL 版本：{version}"
    );
    Ok(series)
}
fn valid_mysql_series(series: &str) -> bool {
    let parts = series.split('.').collect::<Vec<_>>();
    series.len() <= 16
        && parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedisSpec {
    pub package: String,
    pub port: u16,
    #[serde(default, alias = "enabled")]
    pub usage: ServiceUsage,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub virtual_host: bool,
    #[serde(default)]
    pub lan_access: bool,
    #[serde(default = "yes")]
    pub listed: bool,
    pub directory: PathBuf,
    #[serde(default)]
    pub document_root: PathBuf,
    pub php: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
impl Project {
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}
fn yes() -> bool {
    true
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schema: u32,
    pub revision: u64,
    pub http_port: u16,
    #[serde(default = "loopback")]
    pub http_address: String,
    pub apache: Option<String>,
    #[serde(default)]
    pub apache_usage: ServiceUsage,
    pub default_php: Option<String>,
    pub php: Vec<PhpSpec>,
    pub mysql: Vec<MysqlSpec>,
    pub redis: Option<RedisSpec>,
    pub projects: Vec<Project>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema: 1,
            revision: 1,
            http_port: 8080,
            http_address: loopback(),
            apache: None,
            apache_usage: ServiceUsage::Normal,
            default_php: None,
            php: vec![],
            mysql: vec![],
            redis: None,
            projects: vec![],
        }
    }
}
#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    development_root: Option<PathBuf>,
}

impl Layout {
    pub fn prepare(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        ensure!(root.is_absolute(), "环境根目录必须是绝对路径");
        let root = local_path(root, root)?;
        let development_root = std::env::var_os("FLARAGON_DEV_ROOT")
            .map(|value| {
                let value = PathBuf::from(value);
                ensure!(value.is_absolute(), "开发隔离目录必须是绝对路径");
                local_path(&value, &value)
            })
            .transpose()?;
        if let Some(boundary) = &development_root {
            ensure!(path_within(&root, boundary), "环境目录超出开发隔离范围");
        }
        reject_links(&root)?;
        fs::create_dir_all(&root)?;
        let canonical = fs::canonicalize(&root)?;
        Ok(Self {
            root: PathBuf::from(canonical.to_string_lossy().trim_start_matches(r"\\?\")),
            development_root,
        })
    }
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let this = Self::prepare(root)?;
        crate::deployment::ensure_ready(&this.root)?;
        this.initialize()?;
        Ok(this)
    }
    pub fn initialize(&self) -> Result<()> {
        for dir in [
            "bin/apache",
            "bin/php",
            "bin/mysql",
            "bin/redis",
            "config/overrides/php",
            "config/overrides/mysql",
            "data/mysql",
            "data/redis",
            "runtime/generated",
            "runtime/temp",
            "runtime/state",
            "logs",
            "www",
        ] {
            fs::create_dir_all(self.path(dir)?)?;
        }
        Ok(())
    }
    pub fn path(&self, value: impl AsRef<Path>) -> Result<PathBuf> {
        let joined = local_path(&self.root, value.as_ref())?;
        ensure!(
            path_within(&joined, &self.root),
            "路径不在环境根目录内：{}",
            joined.display()
        );
        reject_links(&joined)?;
        Ok(joined)
    }
    /// Business directories are independent of the environment's managed storage.
    pub fn site_path(&self, value: impl AsRef<Path>) -> Result<PathBuf> {
        let joined = local_path(&self.root, value.as_ref())?;
        if let Some(boundary) = &self.development_root {
            ensure!(
                path_within(&joined, boundary),
                "网站目录超出开发隔离范围：{}",
                joined.display()
            );
        }
        reject_links(&joined)?;
        Ok(joined)
    }
    pub fn project_root(&self, project: &Project) -> Result<PathBuf> {
        let directory = self.site_path(&project.directory)?;
        ensure!(
            project
                .document_root
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
            "网站入口必须是项目内的相对路径"
        );
        self.site_path(directory.join(&project.document_root))
    }
    pub fn development_root(&self) -> Option<&Path> {
        self.development_root.as_deref()
    }
    pub fn load(&self) -> Result<Config> {
        let file = self.path("config/app.json")?;
        if !file.exists() {
            return Ok(Config::default());
        }
        let config: Config =
            serde_json::from_slice(&fs::read(file)?).context("无法读取 app.json")?;
        config.validate(self)?;
        Ok(config)
    }
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate(self)?;
        let mut config = config.clone();
        config.relativize_paths(self)?;
        atomic_write_if_changed(
            &self.path("config/app.json")?,
            &serde_json::to_vec_pretty(&config)?,
        )
    }
    pub fn relative(&self, value: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.path(value)?;
        relative_to(&path, &self.root).context("无法取得环境内相对路径")
    }
    pub fn php_ini(&self, package: &str) -> Result<PathBuf> {
        valid_id(package)?;
        self.path(format!("config/overrides/php/{package}.ini"))
    }
}

/// Normalize local Windows paths without accessing the filesystem.
pub fn local_path(base: &Path, value: &Path) -> Result<PathBuf> {
    let p = value;
    let text = p.as_os_str().to_string_lossy();
    ensure!(!text.chars().any(char::is_control), "路径包含控制字符");
    for component in p.components() {
        if let Component::Normal(name) = component {
            let name = name.to_string_lossy();
            ensure!(
                !name.contains([':', '"', '<', '>', '|', '*', '?']) && !name.ends_with(['.', ' ']),
                "路径包含流名称或含糊的目录名"
            );
            let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
            ensure!(
                !matches!(
                    base.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) && !(base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && base.as_bytes()[3].is_ascii_digit()),
                "路径包含 Windows 设备名称"
            );
        }
    }
    ensure!(!p.has_root() || p.is_absolute(), "不允许省略盘符的绝对路径");
    ensure!(
        !matches!(p.components().next(), Some(Component::Prefix(_))) || p.is_absolute(),
        "不允许盘符相对路径"
    );
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    ensure!(
        matches!(joined.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_))),
        "请选择本机盘符目录，不支持网络或设备路径"
    );
    ensure!(
        !joined
            .components()
            .any(|p| matches!(p, Component::ParentDir)),
        "不允许父目录跳转：{}",
        joined.display()
    );
    Ok(joined.components().collect())
}
pub(crate) fn path_within(path: &Path, base: &Path) -> bool {
    let check = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let prefix = base
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    check == prefix || check.starts_with(&(prefix + "\\"))
}

/// Compare Windows path components without touching the old location on disk.
pub(crate) fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    if !path_within(path, base) {
        return None;
    }
    let suffix: PathBuf = path.components().skip(base.components().count()).collect();
    Some(if suffix.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        suffix
    })
}

pub fn reject_links(path: &Path) -> Result<()> {
    // Inspect parents before descendants so a junction is rejected before traversing it.
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let meta = match fs::symlink_metadata(ancestor) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("无法校验目录：{}", ancestor.display()));
            }
        };
        {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    meta.file_attributes() & 0x400 == 0,
                    "拒绝重解析点：{}",
                    ancestor.display()
                );
            }
            ensure!(
                !meta.file_type().is_symlink(),
                "拒绝符号链接：{}",
                ancestor.display()
            );
        }
    }
    Ok(())
}
pub fn valid_id(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 80
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "标识仅允许字母、数字、横线、下划线（1–80 字符）"
    );
    Ok(())
}
pub fn valid_domain(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 253
            && s.contains('.')
            && s.split('.').all(|part| !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')),
        "域名无效：{s}"
    );
    Ok(())
}

impl Config {
    pub(crate) fn relativize_paths(&mut self, layout: &Layout) -> Result<()> {
        for value in self.projects.iter_mut().map(|p| &mut p.directory).chain(
            self.php
                .iter_mut()
                .map(|p| &mut p.directory_entry.directory),
        ) {
            let path = layout.site_path(&*value)?;
            if let Some(relative) = relative_to(&path, &layout.root) {
                *value = relative;
            }
        }
        Ok(())
    }
    pub fn service_usage(&self, id: &str) -> Result<ServiceUsage> {
        if id == "apache" {
            return Ok(self.apache_usage);
        }
        if id == "redis" {
            return self.redis.as_ref().map(|r| r.usage).context("Redis 不存在");
        }
        if let Some(package) = id.strip_prefix("php:") {
            return self
                .php
                .iter()
                .find(|p| p.package == package)
                .map(|p| p.usage)
                .context("PHP 不存在");
        }
        if let Some(name) = id.strip_prefix("mysql:") {
            return self
                .mysql
                .iter()
                .find(|m| m.id == name)
                .map(|m| m.usage)
                .context("MySQL 不存在");
        }
        bail!("服务不存在")
    }
    pub fn apache_dependencies(&self) -> Result<Vec<String>> {
        let mut ids = std::collections::BTreeSet::new();
        for php in self.php.iter().filter(|p| p.directory_entry.enabled) {
            ensure!(
                php.usage != ServiceUsage::Disabled,
                "目录入口所需 PHP 已停用：{}",
                php.package
            );
            ids.insert(format!("php:{}", php.package));
        }
        for project in self.projects.iter().filter(|p| p.enabled) {
            let package = project
                .php
                .as_ref()
                .or(self.default_php.as_ref())
                .context("项目未选择 PHP")?;
            let id = format!("php:{package}");
            ensure!(
                self.service_usage(&id)? != ServiceUsage::Disabled,
                "项目 {} 所需 PHP 已停用，请先在环境设置中调整使用方式",
                project.id
            );
            ids.insert(id);
        }
        Ok(ids.into_iter().collect())
    }
    pub fn startup_ids(&self, common_only: bool) -> Result<Vec<String>> {
        let selected = |usage| {
            usage != ServiceUsage::Disabled && (!common_only || usage == ServiceUsage::Common)
        };
        let mut ids = Vec::new();
        ids.extend(
            self.mysql
                .iter()
                .filter(|m| selected(m.usage))
                .map(|m| format!("mysql:{}", m.id)),
        );
        if self.redis.as_ref().is_some_and(|r| selected(r.usage)) {
            ids.push("redis".into());
        }
        ids.extend(
            self.php
                .iter()
                .filter(|p| selected(p.usage))
                .map(|p| format!("php:{}", p.package)),
        );
        if self.apache.is_some() && selected(self.apache_usage) {
            for id in self.apache_dependencies()? {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            ids.push("apache".into());
        }
        Ok(ids)
    }
    pub fn validate_settings_change(&self, next: &Self, running: &[String]) -> Result<()> {
        for id in running {
            let unchanged = if id == "apache" {
                self.apache == next.apache
                    && self.apache_usage == next.apache_usage
                    && self.http_port == next.http_port
                    && self.http_address == next.http_address
                    && self
                        .php
                        .iter()
                        .map(|p| (&p.package, &p.directory_entry))
                        .collect::<Vec<_>>()
                        == next
                            .php
                            .iter()
                            .map(|p| (&p.package, &p.directory_entry))
                            .collect::<Vec<_>>()
                    && self.default_php == next.default_php
            } else if id == "redis" {
                self.redis == next.redis
            } else if let Some(package) = id.strip_prefix("php:") {
                self.php.iter().find(|p| p.package == package)
                    == next.php.iter().find(|p| p.package == package)
            } else if let Some(name) = id.strip_prefix("mysql:") {
                self.mysql.iter().find(|m| m.id == name) == next.mysql.iter().find(|m| m.id == name)
            } else {
                false
            };
            ensure!(unchanged, "{id} 正在运行，停止后才能编辑、停用或移除");
        }
        if running.iter().any(|id| id == "apache") {
            for id in self.apache_dependencies()? {
                let package = id.trim_start_matches("php:");
                ensure!(
                    self.php.iter().find(|p| p.package == package)
                        == next.php.iter().find(|p| p.package == package),
                    "运行中的网站正在使用 {id}，请先停止 Apache"
                );
            }
        }
        for old in &self.mysql {
            if let Some(new) = next.mysql.iter().find(|m| m.id == old.id) {
                ensure!(
                    old.name == new.name && old.series == new.series && old.package == new.package,
                    "已保存的 MySQL 数据目录和版本不能改名或切换，请移除服务后重新添加"
                );
            }
        }
        Ok(())
    }
    pub fn validate(&self, layout: &Layout) -> Result<()> {
        ensure!(self.schema == 1, "不支持的配置版本");
        listen_addresses(&self.http_address)?;
        let mut ports = BTreeMap::new();
        let mut port = |n, owner: String| -> Result<()> {
            ensure!(n > 0, "端口不能为 0");
            if let Some(other) = ports.insert(n, owner.clone()) {
                bail!("端口 {n} 重复：{other} / {owner}");
            }
            Ok(())
        };
        port(self.http_port, "Apache".into())?;
        let mut ids = std::collections::BTreeSet::new();
        for spec in &self.php {
            valid_id(&spec.package)?;
            ensure!(ids.insert(spec.package.clone()), "PHP 配置重复");
            ensure!((1..=8).contains(&spec.workers), "PHP worker 数量应为 1–8");
            if spec.directory_entry.enabled {
                port(
                    spec.directory_entry.port,
                    format!("{} 目录入口", spec.package),
                )?;
                listen_addresses(&spec.directory_entry.address)?;
                ensure!(
                    !spec.directory_entry.directory.as_os_str().is_empty(),
                    "目录入口根目录不能为空"
                );
                layout.site_path(&spec.directory_entry.directory)?;
            }
            for offset in 0..spec.workers {
                port(
                    spec.port.checked_add(offset).context("PHP 端口超出范围")?,
                    spec.package.clone(),
                )?;
            }
        }
        ids.clear();
        let mut data_paths = std::collections::BTreeSet::new();
        for spec in &self.mysql {
            valid_id(&spec.id)?;
            valid_id(&spec.package)?;
            ensure!(ids.insert(spec.id.to_lowercase()), "数据库实例标识重复");
            if !spec.series.is_empty() {
                let path = spec.data_path(layout)?;
                ensure!(
                    data_paths.insert(path.to_string_lossy().to_lowercase()),
                    "同一 MySQL 数据目录不能被多个服务使用：{}",
                    path.display()
                );
            }
            port(spec.port, spec.id.clone())?;
        }
        if let Some(redis) = &self.redis {
            valid_id(&redis.package)?;
            port(redis.port, "Redis".into())?;
        }
        ids.clear();
        let mut domains = std::collections::BTreeSet::new();
        for project in &self.projects {
            valid_id(&project.id)?;
            ensure!(
                !project.id.to_ascii_lowercase().starts_with("_flaragon"),
                "项目访问标识使用了保留前缀"
            );
            ensure!(
                project.name.chars().count() <= 100
                    && project.group.chars().count() <= 60
                    && !project.name.chars().any(char::is_control)
                    && !project.group.chars().any(char::is_control),
                "项目名称或分组无效"
            );
            if project.virtual_host {
                valid_domain(&project.domain)?;
                ensure!(
                    domains.insert(project.domain.to_lowercase()),
                    "项目域名重复"
                );
            }
            ensure!(ids.insert(project.id.to_lowercase()), "项目访问标识重复");
            ensure!(
                !project.directory.as_os_str().is_empty(),
                "项目目录不能为空"
            );
            layout.project_root(project)?;
            let php = project.php.as_ref().or(self.default_php.as_ref());
            ensure!(
                php.is_some_and(|id| self.php.iter().any(|p| &p.package == id)),
                "项目 {} 未选择有效 PHP",
                project.id
            );
        }
        Ok(())
    }
    pub fn http_listeners(&self) -> Result<Vec<(std::net::Ipv4Addr, u16)>> {
        let mut values = listen_addresses(&self.http_address)?
            .into_iter()
            .map(|ip| (ip, self.http_port))
            .collect::<Vec<_>>();
        for p in self.php.iter().filter(|p| p.directory_entry.enabled) {
            values.extend(
                listen_addresses(&p.directory_entry.address)?
                    .into_iter()
                    .map(|ip| (ip, p.directory_entry.port)),
            );
        }
        Ok(values)
    }
    pub fn http_ports(&self) -> Vec<u16> {
        std::iter::once(self.http_port)
            .chain(
                self.php
                    .iter()
                    .filter(|p| p.directory_entry.enabled)
                    .map(|p| p.directory_entry.port),
            )
            .collect()
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_links(path)?;
    let tmp = path.with_extension("pending");
    reject_links(&tmp)?;
    {
        use std::io::Write;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    crate::platform::replace_file(&tmp, path).context("原子保存失败")
}
pub fn atomic_write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_links(path)?;
    match fs::read(path) {
        Ok(current) if current == bytes => Ok(()),
        Ok(_) => atomic_write(path, bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => atomic_write(path, bytes),
        Err(error) => Err(error.into()),
    }
}
pub fn apache_path(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .context("路径无法表示为 UTF-8")?
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    ensure!(!s.contains(['"', '\n', '\r', '$']), "路径含配置保留字符");
    Ok(s)
}
