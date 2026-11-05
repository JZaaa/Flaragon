use crate::{
    Context, Result, bail,
    config::*,
    discovery, ensure, mysql_network,
    platform::{self, Identity, Process},
    protocol, service_config,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub state: String,
    pub ports: String,
    pub detail: String,
    pub running: bool,
    pub missing: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    kind: Kind,
    identity: Identity,
    ports: Vec<u16>,
    config: PathBuf,
    #[serde(default)]
    mysql_dual_loopback: bool,
    #[serde(default)]
    listeners: Vec<SocketAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    redis_auth: Option<Vec<u8>>,
}
struct Running {
    record: Record,
    process: Process,
}
impl Running {
    fn listening(&self) -> bool {
        if !self.record.listeners.is_empty() {
            return self
                .record
                .listeners
                .iter()
                .all(|address| self.process.owns_listener(*address).unwrap_or(false));
        }
        self.record.ports.iter().all(|port| {
            if self.record.kind == Kind::Mysql {
                mysql_network::listeners(*port, self.record.mysql_dual_loopback)
                    .into_iter()
                    .all(|address| self.process.owns_listener(address).unwrap_or(false))
            } else {
                self.process.owns_port(*port).unwrap_or(false)
            }
        })
    }
    fn control_address(&self) -> Result<SocketAddr> {
        let address = self
            .record
            .listeners
            .first()
            .copied()
            .unwrap_or_else(|| (Ipv4Addr::LOCALHOST, self.record.ports[0]).into());
        ensure!(
            self.process.owns_listener(address)?,
            "监听地址 {address} 不属于本实例，拒绝发送控制命令"
        );
        Ok(crate::service_network::control(address))
    }
}
#[derive(Serialize, Deserialize)]
struct RuntimeState {
    records: Vec<Record>,
    token: String,
    applied: Option<Config>,
    #[serde(default)]
    applied_files: BTreeMap<PathBuf, Vec<u8>>,
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub addresses: Vec<String>,
    pub config: Config,
    pub packages: Vec<Package>,
    pub rows: Vec<Row>,
    pub applied_revision: Option<u64>,
    pub message: String,
}
pub struct Engine {
    pub layout: Layout,
    pub config: Config,
    pub packages: Vec<Package>,
    running: BTreeMap<String, Vec<Running>>,
    pub message: String,
    errors: BTreeMap<String, String>,
    applied: Option<Config>,
    applied_files: BTreeMap<PathBuf, Vec<u8>>,
    token: String,
    discovery_cache: discovery::Cache,
    addresses: Vec<String>,
    mysql_adoption: Option<(MysqlAdoptionReview, crate::mysql_data::Stamp, Vec<u8>)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MysqlAdoptionReview {
    pub token: String,
    pub spec: MysqlSpec,
    pub version: String,
    pub description: String,
}

#[derive(Deserialize)]
pub struct MysqlAdoptionConfirmation {
    pub token: String,
    pub source_version: String,
    pub source_stopped: bool,
    pub backup_confirmed: bool,
    pub config_confirmed: bool,
}
impl Engine {
    fn missing_package(&self, config: &Config, id: &str) -> bool {
        config.service_package(id).is_ok_and(|(kind, package)| {
            !self
                .packages
                .iter()
                .any(|p| p.kind == kind && p.id == package)
        })
    }

    fn deferred_config(&self, config: &Config, id: &str) -> Result<bool> {
        if self.missing_package(config, id) {
            return Ok(true);
        }
        Ok(id == "apache"
            && config
                .apache_dependencies()?
                .iter()
                .any(|id| self.missing_package(config, id)))
    }

    pub fn remove_missing_service(&mut self, id: &str) -> Result<()> {
        self.config.service_package(id)?;
        // Recheck discovery without adding or selecting services as a side effect.
        self.packages = discovery::scan_cached(&self.layout, &mut self.discovery_cache)?;
        ensure!(
            self.missing_package(&self.config, id),
            "组件仍存在或已恢复，请刷新列表；只能移除缺失组件"
        );
        let mut candidate = self.config.without_service(id)?;
        self.config
            .validate_settings_change(&candidate, &self.running_ids())?;
        candidate.revision = self
            .config
            .revision
            .checked_add(1)
            .context("配置版本号超出范围")?;
        candidate.validate(&self.layout)?;
        atomic_write(
            &self.layout.path("config/app.backup.json")?,
            &serde_json::to_vec_pretty(&self.config)?,
        )?;
        self.layout.save(&candidate)?;
        self.config = candidate;
        self.running.remove(id);
        self.errors.remove(id);
        self.applied = None;
        // Deleted files must not be recreated by a later configuration rollback.
        self.applied_files
            .retain(|path, _| self.layout.path(path).is_ok_and(|path| path.is_file()));
        self.message = "已移除缺失组件的服务记录；项目、配置文件和数据均保留".into();
        self.persist()
            .context("移除已保存，但运行状态记录更新失败")?;
        Ok(())
    }

    pub fn switch_project_php(
        &mut self,
        source: &str,
        target: &str,
        remove_missing: bool,
    ) -> Result<()> {
        ensure!(source != target, "请选择另一个 PHP 包");
        self.packages = discovery::scan_cached(&self.layout, &mut self.discovery_cache)?;
        self.package(target, Kind::Php)?;
        ensure!(
            self.config.service_usage(&format!("php:{target}"))? != ServiceUsage::Disabled,
            "目标 PHP 已停用，请先启用"
        );
        let source_id = format!("php:{source}");
        self.config.service_package(&source_id)?;
        if remove_missing {
            ensure!(
                self.missing_package(&self.config, &source_id),
                "原 PHP 组件仍存在，请刷新列表后重试"
            );
            ensure!(
                !self.running_ids().contains(&source_id),
                "原 PHP 仍在运行，请先停止"
            );
        }
        let mut candidate = self.config.clone();
        let mut count = 0;
        for project in &mut candidate.projects {
            if project
                .php
                .as_ref()
                .or(candidate.default_php.as_ref())
                .is_some_and(|id| id == source)
            {
                project.php = Some(target.to_owned());
                count += 1;
            }
        }
        ensure!(count > 0, "没有项目使用此 PHP 包");
        self.apply(candidate)?;
        let applied_message = self.message.clone();
        if remove_missing {
            self.remove_missing_service(&source_id)
                .context("项目已切换，但原 PHP 记录未移除，请检查状态后重试移除")?;
        }
        self.message = format!(
            "已将 {count} 个项目切换到 {target}{}",
            if remove_missing {
                "，并移除原缺失 PHP 记录；项目文件保留"
            } else {
                ""
            }
        );
        self.message.push_str(&format!("；{applied_message}"));
        Ok(())
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.running
            .iter()
            .filter(|(_, entries)| entries.iter().any(|p| p.process.running()))
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub fn inspect_mysql(&self, spec: &MysqlSpec) -> Result<bool> {
        use crate::mysql_data::State;
        let report = self.mysql_directory_report(spec)?;
        ensure!(
            matches!(report.state, State::Missing | State::Empty | State::Bound),
            "{}",
            report.message
        );
        Ok(report.path.exists())
    }
    pub fn mysql_directory_report(&self, spec: &MysqlSpec) -> Result<crate::mysql_data::Report> {
        let package = self.package(&spec.package, Kind::Mysql)?;
        ensure!(
            mysql_series(&package.version)? == spec.series,
            "MySQL 组件与数据目录版本不匹配"
        );
        crate::mysql_data::inspect(&self.layout, spec, package)
    }
    fn stopped_mysql_spec(&self, id: &str) -> Result<MysqlSpec> {
        let spec = self
            .config
            .mysql
            .iter()
            .find(|s| s.id == id)
            .context("请先保存 MySQL 服务")?
            .clone();
        ensure!(
            !self.running_ids().contains(&format!("mysql:{id}")),
            "MySQL 正在运行，请先正常停止"
        );
        ensure!(
            spec.usage != ServiceUsage::Disabled,
            "服务已停用，请先启用并保存设置"
        );
        Ok(spec)
    }
    pub fn review_mysql_adoption(&mut self, id: &str) -> Result<MysqlAdoptionReview> {
        self.mysql_adoption = None;
        let spec = self.stopped_mysql_spec(id)?;
        let report = self.mysql_directory_report(&spec)?;
        ensure!(report.can_adopt, "{}", report.message);
        let package = self.package(&spec.package, Kind::Mysql)?;
        ensure!(
            package.version.starts_with("5.7.") || package.version.starts_with("8."),
            "此 MySQL 构建尚不支持接入验证"
        );
        let version = package.version.clone();
        let executable = self.layout.path(&package.executable)?;
        let _guard = crate::mysql_data::exclusive_data_file(&self.layout, &spec)?;
        let candidate = self.mysql_config(&spec)?;
        let mut stamp = crate::mysql_data::stamp(&self.layout, &report.path)?;
        stamp
            .0
            .extend(crate::mysql_data::stamp(&self.layout, &executable)?.0);
        let review = MysqlAdoptionReview {
            token: platform::random_token()?,
            spec,
            version: version.clone(),
            description: format!(
                "目录：{}\n所选版本：{}\n状态：{}\n版本线索：{}\n\n请核对实例配置与来源的表空间、大小写及插件设置。验证将启动 MySQL，再正常关闭；可能写入恢复日志和数据文件，不执行初始化、SQL 导入或升级。备份须由您提前完成。启动成功不代表已校验全部业务表。",
                report.path.display(),
                version,
                report.message,
                report
                    .version_hint
                    .as_deref()
                    .unwrap_or("未找到可确认版本的记录，请填写已核实的完整来源版本")
            ),
        };
        self.mysql_adoption = Some((review.clone(), stamp, fs::read(candidate)?));
        Ok(review)
    }
    pub fn adopt_mysql(&mut self, confirmation: MysqlAdoptionConfirmation) -> Result<MysqlSpec> {
        let (review, expected, config_bytes) =
            self.mysql_adoption.take().context("请重新检查接入目录")?;
        ensure!(
            confirmation.token == review.token,
            "接入确认已失效，请重新检查"
        );
        ensure!(
            confirmation.source_stopped
                && confirmation.backup_confirmed
                && confirmation.config_confirmed,
            "请确认来源正常停止、完整备份及配置匹配"
        );
        ensure!(
            confirmation.source_version.trim() == review.version,
            "来源完整版本必须与所选 MySQL 版本一致；接入不执行升级或降级"
        );
        let spec = self.stopped_mysql_spec(&review.spec.id)?;
        ensure!(spec == review.spec, "服务设置已变化，请重新检查");
        let report = self.mysql_directory_report(&spec)?;
        ensure!(report.can_adopt, "{}", report.message);
        let package = self.package(&spec.package, Kind::Mysql)?.clone();
        ensure!(
            package.version == review.version,
            "组件版本已变化，请重新检查"
        );
        let (_directory, _) = platform::deployment::directory_guard(&report.path)?;
        let exclusive = crate::mysql_data::exclusive_data_file(&self.layout, &spec)?;
        let mut current = crate::mysql_data::stamp(&self.layout, &report.path)?;
        current
            .0
            .extend(crate::mysql_data::stamp(&self.layout, &package.executable)?.0);
        ensure!(
            current == expected,
            "检查后数据目录或组件发生变化，请重新检查"
        );
        let candidate = self.mysql_config(&spec)?;
        ensure!(
            fs::read(&candidate)? == config_bytes,
            "检查后配置发生变化，请重新检查"
        );
        self.preflight_database(
            Kind::Mysql,
            &package.executable,
            &package.directory,
            &candidate,
            spec.port,
        )?;
        let conf = self.commit_service_config(&format!("mysql:{}", spec.id), &candidate)?;
        let pending = self
            .layout
            .path(report.path.join(crate::mysql_data::ADOPTING))?;
        atomic_write(
            &pending,
            &crate::mysql_data::binding_bytes(&package, "adopted")?,
        )?;
        drop(exclusive);
        let id = format!("mysql:{}", spec.id);
        let attempt = (|| -> Result<()> {
            let mut args = vec![
                format!("--defaults-file={}", conf.display()),
                "--console".into(),
                "--event-scheduler=OFF".into(),
                "--skip-slave-start".into(),
            ];
            if package.version.starts_with("8.") {
                args.push("--upgrade=NONE".into());
            }
            self.launch(
                &id,
                Kind::Mysql,
                &package.executable,
                &package.directory,
                args,
                vec![spec.port],
                conf,
            )?;
            ensure!(
                crate::discovery::version(&self.mysql_greeting(&id)?) == package.version,
                "接入验证返回的 MySQL 版本不符"
            );
            self.stop_internal(&id)?;
            ensure!(
                !spec.binding_path(&self.layout)?.exists(),
                "验证期间出现其他绑定信息，拒绝覆盖"
            );
            crate::mysql_data::publish_binding(&self.layout, &spec, &package, "adopted")?;
            fs::remove_file(&pending)?;
            Ok(())
        })();
        if let Err(error) = attempt {
            let stopped = self
                .stop_internal(&id)
                .map(|_| "验证进程已停止".to_string())
                .unwrap_or_else(|e| format!("正常停止未完成：{e:#}；请通过服务停止操作重试"));
            bail!(
                "接入未完成：{error:#}；{stopped}。数据保留，不重新初始化；请查看实例日志并重新检查。"
            );
        }
        self.message =
            "已有 MySQL 数据接入成功，启动与正常关闭已验证；服务当前已停止，可正常启动".into();
        Ok(spec)
    }
    fn validate_settings(&self, candidate: &Config) -> Result<()> {
        self.config
            .validate_settings_change(candidate, &self.running_ids())?;
        for spec in &candidate.mysql {
            let old = self.config.mysql.iter().find(|old| old.id == spec.id);
            if old.is_none_or(|old| {
                old.name != spec.name || old.series != spec.series || old.package != spec.package
            }) {
                let report = self.mysql_directory_report(spec)?;
                let exists = if report.can_adopt {
                    true
                } else {
                    self.inspect_mysql(spec)?
                };
                ensure!(
                    !exists || spec.reuse_data,
                    "数据目录已存在，请确认使用历史数据：{}",
                    spec.data_path(&self.layout)?.display()
                );
            }
        }
        Ok(())
    }
    pub fn apply_settings(&mut self, mut candidate: Config) -> Result<()> {
        candidate.validate(&self.layout)?;
        self.validate_settings(&candidate)?;
        ensure!(
            serde_json::to_value(&candidate.projects)?
                == serde_json::to_value(&self.config.projects)?,
            "请通过项目页面修改项目"
        );
        let old = self.config.clone();
        candidate.revision = old.revision.checked_add(1).context("配置版本号超出范围")?;
        let changed: Vec<_> = self
            .service_ids_for(&candidate)
            .into_iter()
            .filter(|id| {
                if id == "apache" {
                    old.apache != candidate.apache
                        || old.http_port != candidate.http_port
                        || old.http_address != candidate.http_address
                        || old
                            .php
                            .iter()
                            .map(|p| (&p.package, &p.directory_entry))
                            .collect::<Vec<_>>()
                            != candidate
                                .php
                                .iter()
                                .map(|p| (&p.package, &p.directory_entry))
                                .collect::<Vec<_>>()
                        || old.default_php != candidate.default_php
                } else if id == "redis" {
                    old.redis != candidate.redis
                } else if let Some(p) = id.strip_prefix("php:") {
                    old.php.iter().find(|s| s.package == p)
                        != candidate.php.iter().find(|s| s.package == p)
                } else {
                    let name = id.trim_start_matches("mysql:");
                    old.mysql.iter().find(|s| s.id == name)
                        != candidate.mysql.iter().find(|s| s.id == name)
                }
            })
            .collect();
        let mut files = BTreeMap::new();
        let deferred = changed
            .iter()
            .filter_map(|id| match self.deferred_config(&candidate, id) {
                Ok(true) => Some(Ok(id.clone())),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<BTreeSet<_>>>()?;
        for id in &changed {
            if self.missing_package(&candidate, id) {
                continue;
            }
            files.extend(service_config::snapshot(
                &self.layout,
                &self.service_config_path_for(&candidate, id)?,
            )?);
        }
        self.config = candidate;
        let result: Result<()> = (|| {
            self.ensure_overrides()?;
            let prepared = changed
                .iter()
                .filter(|id| {
                    self.config
                        .service_usage(id)
                        .is_ok_and(|u| u != ServiceUsage::Disabled)
                        && !deferred.contains(*id)
                })
                .map(|id| self.service_candidate(id).map(|path| (id, path)))
                .collect::<Result<Vec<_>>>()?;
            atomic_write(
                &self.layout.path("config/app.backup.json")?,
                &serde_json::to_vec_pretty(&old)?,
            )?;
            for (id, path) in prepared {
                self.commit_service_config(id, &path)?;
            }
            self.layout.save(&self.config)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.config = old;
            let mut errors = Vec::new();
            for (path, bytes) in files {
                if let Err(e) = atomic_write(&path, &bytes) {
                    errors.push(e.to_string());
                }
            }
            bail!(
                "设置未保存：{error:#}；恢复结果：{}",
                if errors.is_empty() {
                    "已恢复".into()
                } else {
                    errors.join("；")
                }
            );
        }
        self.message = if deferred.is_empty() {
            "环境设置已保存；运行中的服务保持运行".into()
        } else {
            "环境设置已保存；缺失组件关联配置尚未校验，运行中的服务保持运行".into()
        };
        self.persist()?;
        Ok(())
    }
    fn service_ids_for(&self, config: &Config) -> Vec<String> {
        let mut ids = Vec::new();
        if config.apache.is_some() {
            ids.push("apache".into());
        }
        ids.extend(config.php.iter().map(|s| format!("php:{}", s.package)));
        ids.extend(config.mysql.iter().map(|s| format!("mysql:{}", s.id)));
        if config.redis.is_some() {
            ids.push("redis".into());
        }
        ids
    }
    pub fn new(layout: Layout) -> Result<Self> {
        let paths = crate::relocation::check(&layout)?;
        let config = layout.load()?;
        let mut engine = Self {
            layout,
            config,
            packages: vec![],
            running: BTreeMap::new(),
            message: paths.message(),
            errors: BTreeMap::new(),
            applied: None,
            applied_files: BTreeMap::new(),
            token: platform::random_token()?,
            discovery_cache: discovery::Cache::default(),
            addresses: platform::local_ipv4_addresses()?,
            mysql_adoption: None,
        };
        engine.recover()?;
        Ok(engine)
    }
    pub fn scan(&mut self) -> Result<()> {
        self.addresses = platform::local_ipv4_addresses()?;
        self.packages = discovery::scan_cached(&self.layout, &mut self.discovery_cache)?;
        let mut config = self.config.clone();
        for p in self.packages.iter().filter(|p| p.error.is_none()) {
            match p.kind {
                Kind::Apache => {
                    if config.apache.is_none() {
                        config.apache = Some(p.id.clone());
                    }
                }
                Kind::Php => {
                    if config.default_php.is_none() {
                        config.default_php = Some(p.id.clone());
                    }
                    if !config.php.iter().any(|s| s.package == p.id) {
                        config.php.push(PhpSpec {
                            package: p.id.clone(),
                            port: next_port(&config, 19000, 2)?,
                            workers: 2,
                            usage: ServiceUsage::Normal,
                            directory_entry: DirectoryEntry::default(),
                        });
                    }
                }
                Kind::Mysql => {}
                Kind::Redis => {
                    if config.redis.is_none() {
                        config.redis = Some(RedisSpec {
                            package: p.id.clone(),
                            port: next_port(&config, 6379, 1)?,
                            usage: ServiceUsage::Normal,
                        });
                    }
                }
            }
        }
        self.layout.save(&config)?;
        self.config = config;
        self.ensure_overrides()?;
        self.message = format!(
            "识别 {} 个组件；异常 {} 个",
            self.packages.len(),
            self.packages.iter().filter(|p| p.error.is_some()).count()
        );
        Ok(())
    }
    fn package(&self, id: &str, kind: Kind) -> Result<&Package> {
        let p = self
            .packages
            .iter()
            .find(|p| p.id == id && p.kind == kind)
            .with_context(|| format!("组件不存在，请刷新：{id}"))?;
        ensure!(
            p.error.is_none(),
            "组件不可用：{}",
            p.error.as_deref().unwrap_or_default()
        );
        self.layout.path(&p.executable)?;
        self.layout.path(&p.directory)?;
        Ok(p)
    }
    fn ensure_overrides(&self) -> Result<()> {
        let apache = self.layout.path("config/overrides/apache.conf")?;
        if !apache.exists() {
            atomic_write(&apache,b"# Apache settings: Timeout, KeepAlive, MaxKeepAliveRequests, KeepAliveTimeout\nTimeout 60\nKeepAlive On\n")?;
        }
        Ok(())
    }
    pub fn service_config_path(&self, id: &str) -> Result<PathBuf> {
        self.service_config_path_for(&self.config, id)
    }
    fn service_config_path_for(&self, config: &Config, id: &str) -> Result<PathBuf> {
        if id == "apache" {
            return self.layout.path("config/services/apache/httpd.conf");
        }
        let (package, kind) = if let Some(p) = id.strip_prefix("php:") {
            (p, Kind::Php)
        } else if let Some(instance) = id.strip_prefix("mysql:") {
            (
                config
                    .mysql
                    .iter()
                    .find(|s| s.id == instance)
                    .context("MySQL 服务不存在")?
                    .package
                    .as_str(),
                Kind::Mysql,
            )
        } else if id == "redis" {
            (
                config
                    .redis
                    .as_ref()
                    .context("Redis 服务不存在")?
                    .package
                    .as_str(),
                Kind::Redis,
            )
        } else {
            bail!("未知服务")
        };
        service_config::package_path(&self.layout, self.package(package, kind)?, id)
    }
    fn commit_service_config(&self, id: &str, candidate: &Path) -> Result<PathBuf> {
        service_config::commit(
            &self.layout,
            &self.service_config_path(id)?,
            self.service_kind(id)?,
            candidate,
        )
    }
    pub fn save_override(&mut self, id: &str, text: &str) -> Result<()> {
        self.read_service_config(id)?;
        let full =
            service_config::replace_user(&self.layout, &self.service_config_path(id)?, text)?;
        self.save_service_config(id, &full)
    }
    fn service_kind(&self, id: &str) -> Result<Kind> {
        ensure!(
            self.service_ids().iter().any(|service| service == id),
            "服务不存在"
        );
        Ok(if id == "apache" {
            Kind::Apache
        } else if id == "redis" {
            Kind::Redis
        } else if id.starts_with("php:") {
            Kind::Php
        } else {
            Kind::Mysql
        })
    }
    fn service_candidate(&self, id: &str) -> Result<PathBuf> {
        match self.service_kind(id)? {
            Kind::Apache => self.apache_config(),
            Kind::Redis => self.redis_config(),
            Kind::Php => self.php_config(
                self.config
                    .php
                    .iter()
                    .find(|s| id == format!("php:{}", s.package))
                    .context("PHP 不存在")?,
            ),
            Kind::Mysql => self.mysql_config(
                self.config
                    .mysql
                    .iter()
                    .find(|s| id == format!("mysql:{}", s.id))
                    .context("MySQL 不存在")?,
            ),
        }
    }
    pub fn read_service_config(&self, id: &str) -> Result<String> {
        self.service_kind(id)?;
        if service_config::needs_template(&self.layout, &self.service_config_path(id)?)? {
            let candidate = self.service_candidate(id)?;
            self.commit_service_config(id, &candidate)?;
        }
        service_config::read(&self.layout, &self.service_config_path(id)?)
    }
    pub fn save_service_config(&mut self, id: &str, text: &str) -> Result<()> {
        service_config::validate_edit(
            &self.layout,
            &self.service_config_path(id)?,
            self.service_kind(id)?,
            text,
        )?;
        let file = self.service_config_path(id)?;
        let before = fs::read(&file)?;
        atomic_write(
            &self.layout.path(file.with_extension("before-edit"))?,
            &if id == "redis" {
                platform::secrets::seal(&before)?
            } else {
                before.clone()
            },
        )?;
        atomic_write(&file, text.as_bytes())?;
        let candidate = self.config.clone();
        if let Err(e) = self.apply_selected(candidate, Some(id)) {
            return match atomic_write(&file, &before) {
                Ok(()) => Err(e.context("配置应用失败；编辑前文件已恢复")),
                Err(restore) => Err(e.context(format!("配置应用失败；文件恢复失败：{restore:#}"))),
            };
        }
        Ok(())
    }
    fn configuration_files(&self) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
        let mut result = BTreeMap::new();
        for id in self.service_ids() {
            if self.missing_package(&self.config, &id) {
                continue;
            }
            result.extend(service_config::snapshot(
                &self.layout,
                &self.service_config_path(&id)?,
            )?);
        }
        Ok(result)
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut rows = vec![];
        for id in self.service_ids() {
            let (title, ports) = if id == "apache" {
                (
                    format!(
                        "Apache · {}",
                        self.config.apache.as_deref().unwrap_or("未选择")
                    ),
                    self.config
                        .http_ports()
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            } else if id == "redis" {
                (
                    "Redis · 单实例".into(),
                    self.config
                        .redis
                        .as_ref()
                        .map(|r| r.port.to_string())
                        .unwrap_or_default(),
                )
            } else if let Some(package) = id.strip_prefix("php:") {
                let p = self.config.php.iter().find(|p| p.package == package);
                (
                    format!("PHP · {package}"),
                    p.map(|p| format!("{}–{}", p.port, p.port + p.workers - 1))
                        .unwrap_or_default(),
                )
            } else {
                let spec = self
                    .config
                    .mysql
                    .iter()
                    .find(|m| format!("mysql:{}", m.id) == id);
                (
                    spec.map(|m| format!("MySQL · {} · {}", m.id, m.package))
                        .unwrap_or_default(),
                    spec.map(|m| m.port.to_string()).unwrap_or_default(),
                )
            };
            let entries = self.running.get(&id);
            let alive = entries
                .map(|v| v.iter().filter(|p| p.process.running()).count())
                .unwrap_or(0);
            let expected = if let Some(p) = id.strip_prefix("php:") {
                self.config
                    .php
                    .iter()
                    .find(|s| s.package == p)
                    .map(|s| s.workers as usize)
                    .unwrap_or(1)
            } else {
                1
            };
            let listeners_ready = entries.is_some_and(|v| {
                v.iter()
                    .all(|p| !p.record.ports.is_empty() && p.listening())
            });
            let initializing = entries.is_some_and(|v| {
                v.iter()
                    .any(|p| p.process.running() && p.record.ports.is_empty())
            });
            let missing = self.missing_package(&self.config, &id);
            let state = if missing && alive == 0 {
                "组件缺失"
            } else if self.errors.contains_key(&id) {
                "异常"
            } else if initializing {
                "初始化中"
            } else if alive == expected && listeners_ready {
                "运行中"
            } else if alive == expected {
                "未就绪"
            } else if alive > 0 {
                "部分运行"
            } else if entries.is_some() {
                "已退出"
            } else {
                "已停止"
            };
            let detail = self.errors.get(&id).cloned().unwrap_or_else(|| {
                entries
                    .map(|v| {
                        v.iter()
                            .map(|p| p.record.identity.pid.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            });
            rows.push(Row {
                id,
                title,
                state: state.into(),
                ports,
                detail,
                running: alive > 0,
                missing,
            });
        }
        Snapshot {
            addresses: self.addresses.clone(),
            config: self.config.clone(),
            packages: self.packages.clone(),
            rows,
            applied_revision: self.applied.as_ref().map(|c| c.revision),
            message: self.message.clone(),
        }
    }
    pub fn service_ids(&self) -> Vec<String> {
        self.service_ids_for(&self.config)
    }
    pub fn has_running(&self) -> bool {
        self.running.values().flatten().any(|p| p.process.running())
    }
    pub fn start_all(&mut self) -> Result<()> {
        self.start_selected(false)
    }
    pub fn start_common(&mut self) -> Result<()> {
        self.start_selected(true)
    }
    fn start_selected(&mut self, common_only: bool) -> Result<()> {
        let selected = self.config.startup_ids(common_only)?;
        let before = self.running.keys().cloned().collect::<BTreeSet<_>>();
        let result: Result<()> = (|| {
            for id in selected {
                self.start(&id)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            let mut rollback = vec![];
            let ids = self
                .running
                .keys()
                .filter(|id| !before.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            for id in ids {
                if let Err(err) = self.stop_internal(&id) {
                    rollback.push(err.to_string());
                }
            }
            bail!(
                "启动失败：{e}；清理结果：{}",
                if rollback.is_empty() {
                    "新启动实例已停止".into()
                } else {
                    rollback.join("；")
                }
            );
        }
        self.applied = Some(self.config.clone());
        self.applied_files = self.configuration_files()?;
        self.message = "所需服务已启动并验证".into();
        self.persist()?;
        Ok(())
    }
    pub fn start(&mut self, id: &str) -> Result<()> {
        ensure!(
            self.config.service_usage(id)? != ServiceUsage::Disabled,
            "服务已停用，请先在环境设置中调整使用方式"
        );
        if id == "apache" {
            self.config.apache_dependencies()?;
        }
        let expected = id
            .strip_prefix("php:")
            .and_then(|p| self.config.php.iter().find(|s| s.package == p))
            .map(|s| s.workers as usize)
            .unwrap_or(1);
        if let Some(entries) = self.running.get(id) {
            ensure!(
                !entries
                    .iter()
                    .any(|p| p.process.running() && p.record.ports.is_empty()),
                "{id} 正在初始化，请等待完成并检查日志"
            );
            if entries.len() == expected
                && entries.iter().all(|p| p.process.running() && p.listening())
            {
                return Ok(());
            }
        }
        if self
            .running
            .get(id)
            .is_some_and(|v| v.iter().any(|p| p.process.running()))
        {
            self.stop_internal(id)?;
        }
        self.running.remove(id);
        self.errors.remove(id);
        let result = if id == "apache" {
            self.start_apache()
        } else if id == "redis" {
            self.start_redis()
        } else if let Some(p) = id.strip_prefix("php:") {
            self.start_php(p)
        } else if let Some(m) = id.strip_prefix("mysql:") {
            self.start_mysql(m)
        } else {
            Err(crate::anyhow!("服务不存在"))
        };
        if let Err(ref e) = result {
            self.errors.insert(id.into(), e.to_string());
            self.message = e.to_string();
        } else {
            self.message = format!("{id} 已运行");
            self.applied_files = self.configuration_files()?;
        }
        self.persist()?;
        result
    }
    #[allow(clippy::too_many_arguments)] // All launch inputs are explicit to preserve process isolation.
    fn launch(
        &mut self,
        id: &str,
        kind: Kind,
        exe: &Path,
        cwd: &Path,
        args: Vec<String>,
        ports: Vec<u16>,
        config: PathBuf,
    ) -> Result<()> {
        let database_listeners = if matches!(kind, Kind::Mysql | Kind::Redis) {
            self.preflight_database(kind, exe, cwd, &config, ports[0])?
        } else {
            Vec::new()
        };
        let listeners = if kind == Kind::Apache {
            self.config.http_listeners()?
        } else if matches!(kind, Kind::Mysql | Kind::Redis) {
            vec![]
        } else {
            ports.iter().map(|p| (Ipv4Addr::LOCALHOST, *p)).collect()
        };
        for (address, port) in listeners {
            let listener = TcpListener::bind((address, port))
                .with_context(|| format!("监听 {address}:{port} 不可用，未访问占用服务"))?;
            drop(listener);
        }
        let mut cmd = platform::command(
            &self.layout.path(exe)?,
            &self.layout.root,
            &self.layout.path(cwd)?,
        );
        cmd.args(args);
        if kind == Kind::Php {
            cmd.env("PHP_INI_SCAN_DIR", self.layout.path("runtime/empty-ini")?)
                .env("PHP_FCGI_MAX_REQUESTS", "0");
        }
        let log = self
            .layout
            .path(format!("logs/{}.log", id.replace(':', "-")))?;
        let redis_auth = if kind == Kind::Redis {
            Some(platform::secrets::seal(&crate::redis_config::password(
                &fs::read_to_string(&config)?,
            )?)?)
        } else {
            None
        };
        let process = Process::from_child(platform::spawn_logged(&mut cmd, &log)?)?;
        let record = Record {
            id: id.into(),
            kind,
            identity: process.identity.clone(),
            ports,
            config,
            mysql_dual_loopback: false,
            listeners: database_listeners,
            redis_auth,
        };
        self.running
            .entry(id.into())
            .or_default()
            .push(Running { record, process });
        self.persist()?;
        let started = Instant::now();
        loop {
            let p = self
                .running
                .get(id)
                .and_then(|v| v.last())
                .context("进程记录丢失")?;
            ensure!(p.process.running(), "{id} 已退出，查看 {}", log.display());
            if p.listening() {
                return Ok(());
            }
            ensure!(
                started.elapsed() < Duration::from_secs(40),
                "{id} 启动超时，进程保留以便正常关闭；查看日志"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub fn stop(&mut self, id: &str) -> Result<()> {
        if id.starts_with("php:") {
            ensure!(
                !self
                    .running
                    .get("apache")
                    .is_some_and(|v| v.iter().any(|p| p.process.running()))
                    || !self
                        .config
                        .apache_dependencies()?
                        .iter()
                        .any(|dependency| dependency == id),
                "该 PHP 被运行中的 Apache 站点使用，请先停止 Apache 或全部停止"
            );
        }
        let result = self.stop_internal(id);
        if let Err(ref e) = result {
            self.errors.insert(id.into(), e.to_string());
        }
        result
    }
    fn stop_internal(&mut self, id: &str) -> Result<()> {
        if let Some(entries) = self.running.get(id) {
            for p in entries.iter().filter(|p| p.process.running()) {
                match p.record.kind {
                    Kind::Php => p.process.terminate_php()?,
                    Kind::Mysql => {
                        platform::mysql_shutdown(p.record.identity.pid).or_else(|error| {
                            if let Some(port) = p.record.ports.first() {
                                let server = p.process.listener_process(*port)?;
                                platform::mysql_shutdown(server.identity.pid)
                            } else {
                                Err(error)
                            }
                        })?;
                        p.process.wait(Duration::from_secs(45))?;
                    }
                    Kind::Redis => {
                        let address = p.control_address()?;
                        let password = p
                            .record
                            .redis_auth
                            .as_deref()
                            .map(platform::secrets::open)
                            .transpose()?
                            .unwrap_or_default();
                        let response =
                            protocol::redis_authenticated_at(address, &password, &["SHUTDOWN"]);
                        if let Err(e) = response
                            && p.process.running()
                        {
                            return Err(e);
                        }
                        p.process.wait(Duration::from_secs(20))?;
                    }
                    Kind::Apache => {
                        platform::apache_shutdown(p.record.identity.pid)?;
                        p.process.wait(Duration::from_secs(20))?;
                    }
                }
            }
        }
        self.running.remove(id);
        self.errors.remove(id);
        self.persist()?;
        self.message = format!("{id} 已停止");
        Ok(())
    }
    pub fn stop_all(&mut self) -> Result<()> {
        let mut errors = vec![];
        if let Err(e) = self.stop_internal("apache") {
            errors.push(e.to_string());
        }
        if self
            .running
            .get("apache")
            .is_some_and(|v| v.iter().any(|p| p.process.running()))
        {
            bail!("Apache 未能停止，保留依赖服务：{}", errors.join("；"));
        }
        let ids = self.running.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            if let Err(e) = self.stop_internal(&id) {
                errors.push(format!("{id}: {e}"));
            }
        }
        ensure!(errors.is_empty(), "{}", errors.join("；"));
        self.applied = None;
        self.persist()?;
        self.message = "所有受管理服务已正常停止".into();
        Ok(())
    }
    pub fn restart(&mut self, id: &str) -> Result<()> {
        if id.starts_with("php:") && self.running.contains_key("apache") {
            return self.apply_selected(self.config.clone(), Some(id));
        }
        self.stop(id)?;
        self.start(id)
    }
    pub fn reload(&mut self, id: &str) -> Result<()> {
        ensure!(id == "apache", "该服务不支持完整配置重加载，请使用重启");
        let parent = self
            .running
            .get(id)
            .and_then(|v| v.first())
            .context("Apache 未运行")?;
        ensure!(
            parent.process.owns_port(self.config.http_port)?,
            "Apache 未就绪或端口归属不符"
        );
        ensure!(
            self.applied
                .as_ref()
                .is_some_and(|c| c.revision == self.config.revision),
            "存在未应用设置，请先应用设置"
        );
        let active = parent.record.config.clone();
        let override_path = self.service_config_path("apache")?;
        ensure!(
            active == override_path,
            "当前进程使用旧配置路径，请先重启 Apache"
        );
        let previous = self
            .applied_files
            .get(&active)
            .cloned()
            .context("缺少已应用配置快照，请先重启 Apache")?;
        let previous_override = Some(previous.clone());
        let candidate = match self.apache_config_named("httpd-reload-candidate.conf") {
            Ok(path) => path,
            Err(error) => {
                atomic_write(&active, &previous)?;
                return Err(error.context("新配置检查未通过；文件已恢复，运行服务保持不变"));
            }
        };
        let bytes = fs::read(candidate)?;
        atomic_write(&active.with_extension("backup.conf"), &previous)?;
        if let Err(error) = self.reload_apache_bytes(&active, &bytes) {
            let recovery = self.reload_apache_bytes(&active, &previous);
            if let Err(ref restore) = recovery {
                self.applied = None;
                self.errors.insert(
                    id.into(),
                    format!("重加载失败：{error:#}；恢复失败：{restore:#}"),
                );
            } else if let Some(bytes) = previous_override {
                atomic_write(&override_path, &bytes)?;
                self.errors.remove(id);
            }
            self.persist()?;
            bail!(
                "重加载失败：{error:#}；{}",
                recovery
                    .err()
                    .map(|e| format!("恢复失败：{e:#}"))
                    .unwrap_or_else(|| "上一配置已恢复并验证".into())
            );
        }
        self.applied_files
            .insert(override_path.clone(), fs::read(override_path)?);
        self.errors.remove(id);
        self.message = "Apache 已重加载并验证；主进程、PHP 和数据库保持运行".into();
        self.persist()?;
        Ok(())
    }
    fn reload_apache_bytes(&self, active: &Path, bytes: &[u8]) -> Result<()> {
        let marker = platform::random_token()?;
        let directory = self.generated()?;
        let marker_path = directory.join(format!("reload-{marker}.txt"));
        atomic_write(&marker_path, marker.as_bytes())?;
        let mut candidate_bytes = bytes.to_vec();
        candidate_bytes.extend_from_slice(format!("\nAlias /_flaragon_reload_{marker} \"{}\"\n<Directory \"{}\">\nRequire local\n</Directory>\n", apache_path(&marker_path)?, apache_path(&directory)?).as_bytes());
        let candidate = directory.join("httpd-reload-check.conf");
        atomic_write(&candidate, &candidate_bytes)?;
        let package = self.package(
            self.config.apache.as_deref().context("未选择 Apache")?,
            Kind::Apache,
        )?;
        let mut cmd = platform::command(&package.executable, &self.layout.root, &package.directory);
        cmd.arg("-f").arg(&candidate).arg("-t");
        platform::capture(&mut cmd, Duration::from_secs(10))
            .context("Apache 重加载配置校验失败")?;
        let parent = self
            .running
            .get("apache")
            .and_then(|v| v.first())
            .context("Apache 未运行")?;
        ensure!(
            parent.process.owns_port(self.config.http_port)?,
            "Apache 端口归属不符"
        );
        atomic_write(active, &candidate_bytes)?;
        platform::apache_reload(&parent.process)?;
        let start = Instant::now();
        let host = "localhost";
        loop {
            ensure!(parent.process.running(), "Apache 重加载期间主进程退出");
            if parent.process.owns_port(self.config.http_port)?
                && protocol::http(
                    self.config.http_port,
                    host,
                    &format!("/_flaragon_reload_{marker}"),
                )
                .is_ok_and(|(status, body)| status == 200 && body == marker)
                && self.verify_sites().is_ok()
            {
                atomic_write(active, bytes)?;
                return Ok(());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(20),
                "Apache 重加载验证超时"
            );
            thread::sleep(Duration::from_millis(150));
        }
    }
    fn persist(&self) -> Result<()> {
        let records = self
            .running
            .values()
            .flatten()
            .filter(|p| p.process.running())
            .map(|p| {
                let mut record = p.record.clone();
                record.identity.executable = self.layout.relative(&record.identity.executable)?;
                record.config = self.layout.relative(&record.config)?;
                Ok(record)
            })
            .collect::<Result<Vec<_>>>()?;
        let applied_files = self
            .applied_files
            .iter()
            .map(|(path, bytes)| Ok((self.layout.relative(path)?, platform::secrets::seal(bytes)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let applied = self
            .applied
            .clone()
            .map(|mut config| -> Result<Config> {
                config.relativize_paths(&self.layout)?;
                Ok(config)
            })
            .transpose()?;
        atomic_write(
            &self.layout.path("runtime/state/processes.json")?,
            &serde_json::to_vec_pretty(&RuntimeState {
                records,
                token: self.token.clone(),
                applied,
                applied_files,
            })?,
        )
    }
    fn recover(&mut self) -> Result<()> {
        let path = self.layout.path("runtime/state/processes.json")?;
        if !path.exists() {
            return Ok(());
        }
        let state: RuntimeState = serde_json::from_slice(&fs::read(path)?)
            .context("运行记录损坏；为避免重复启动，请先检查 runtime/state")?;
        ensure!(
            state.token.len() == 32 && state.token.bytes().all(|c| c.is_ascii_hexdigit()),
            "运行配置标识无效"
        );
        self.token = state.token;
        self.applied = state.applied;
        for (path, bytes) in state.applied_files {
            let checked = self.layout.path(&path)?;
            let relative = self.layout.relative(&checked)?;
            if relative.starts_with("config/services")
                && !relative.starts_with("config/services/apache")
                || relative.starts_with("config/overrides")
                    && relative != Path::new("config/overrides/apache.conf")
            {
                self.applied = None;
                continue;
            }
            ensure!(
                crate::relocation::is_config_path(&relative),
                "运行配置快照路径无效"
            );
            self.applied_files
                .insert(checked, platform::secrets::open(&bytes)?);
        }
        for mut record in state.records {
            record.identity.executable = self.layout.path(&record.identity.executable)?;
            record.config = self.layout.path(&record.config)?;
            if let Ok(process) = Process::recover(&record.identity) {
                self.running
                    .entry(record.id.clone())
                    .or_default()
                    .push(Running { record, process });
            }
        }
        if self.has_running() {
            self.message = "已恢复管理本环境正在运行的服务".into();
        }
        Ok(())
    }
    fn generated(&self) -> Result<PathBuf> {
        let path = self.layout.path(format!(
            "runtime/generated/{}-{}",
            self.config.revision, self.token
        ))?;
        fs::create_dir_all(&path)?;
        Ok(path)
    }
    fn php_config(&self, spec: &PhpSpec) -> Result<PathBuf> {
        let package = self.package(&spec.package, Kind::Php)?;
        let out = self.generated()?.join(format!("php-{}.ini", spec.package));
        let session = self
            .layout
            .path(format!("runtime/temp/session-{}", spec.package))?;
        fs::create_dir_all(&session)?;
        let text = format!(
            "[PHP]\nextension_dir=\"{}\"\nsys_temp_dir=\"{}\"\nupload_tmp_dir=\"{}\"\nsession.save_path=\"{}\"\nerror_log=\"{}\"\ncgi.fix_pathinfo=1\ncgi.force_redirect=0\n",
            apache_path(&package.directory.join("ext"))?,
            apache_path(&self.layout.path("runtime/temp")?)?,
            apache_path(&self.layout.path("runtime/temp")?)?,
            apache_path(&session)?,
            apache_path(
                &self
                    .layout
                    .path(format!("logs/php-{}-error.log", spec.package))?
            )?
        );
        service_config::prepare(
            &self.layout,
            &self.service_config_path(&format!("php:{}", spec.package))?,
            Kind::Php,
            &text,
            "",
            "",
            &out,
            Some(&crate::config_template::load(
                &self.layout,
                &package.directory,
                Kind::Php,
                &text,
            )?),
        )?;
        let mut cmd = platform::command(&package.executable, &self.layout.root, &package.directory);
        cmd.env("PHP_INI_SCAN_DIR", self.layout.path("runtime/empty-ini")?)
            .arg("-c")
            .arg(&out)
            .args(["-r", "echo 'FLARAGON_OK';"]);
        let result = platform::capture(&mut cmd, Duration::from_secs(8))?;
        ensure!(
            result.trim() == "FLARAGON_OK",
            "PHP 配置无效：{}",
            result.trim()
        );
        Ok(out)
    }
    fn start_php(&mut self, id: &str) -> Result<()> {
        let spec = self
            .config
            .php
            .iter()
            .find(|p| p.package == id)
            .context("PHP 实例不存在")?
            .clone();
        let package = self.package(id, Kind::Php)?.clone();
        let candidate = self.php_config(&spec)?;
        let ini = self.commit_service_config(&format!("php:{id}"), &candidate)?;
        fs::create_dir_all(self.layout.path("runtime/empty-ini")?)?;
        for offset in 0..spec.workers {
            let port = spec.port + offset;
            self.launch(
                &format!("php:{id}"),
                Kind::Php,
                &package.directory.join("php-cgi.exe"),
                &package.directory,
                vec![
                    "-c".into(),
                    ini.to_string_lossy().into_owned(),
                    "-b".into(),
                    format!("127.0.0.1:{port}"),
                ],
                vec![port],
                ini.clone(),
            )?;
        }
        Ok(())
    }
    fn mysql_greeting(&self, id: &str) -> Result<String> {
        let running = self
            .running
            .get(id)
            .and_then(|entries| entries.first())
            .context("MySQL 未运行")?;
        protocol::mysql_greeting_at(running.control_address()?)
    }
    fn preflight_database(
        &self,
        kind: Kind,
        exe: &Path,
        cwd: &Path,
        config: &Path,
        port: u16,
    ) -> Result<Vec<SocketAddr>> {
        let addresses = if kind == Kind::Mysql {
            let mut cmd = platform::command(exe, &self.layout.root, cwd);
            cmd.arg(format!("--defaults-file={}", config.display()))
                .args(["--verbose", "--help"]);
            let output = platform::capture_bounded(&mut cmd, Duration::from_secs(10), 1024 * 1024)?;
            crate::service_network::mysql(&output, port)?
                .into_iter()
                .map(|address| (address, false))
                .collect()
        } else {
            crate::service_network::redis(&fs::read_to_string(config)?, port)?
        };
        crate::service_network::preflight(&addresses)
    }
    fn mysql_config(&self, spec: &MysqlSpec) -> Result<PathBuf> {
        let p = self.package(&spec.package, Kind::Mysql)?;
        let dir = spec.data_path(&self.layout)?;
        ensure!(
            mysql_series(&p.version)? == spec.series,
            "MySQL 数据目录版本与组件不匹配"
        );
        let out = self.generated()?.join(format!("mysql-{}.ini", spec.id));
        let mut text = format!(
            "[mysqld]\nbasedir=\"{}\"\ndatadir=\"{}\"\nport={}\nlog-error=\"{}\"\npid-file=\"{}\"\ntmpdir=\"{}\"\nsecure-file-priv=NULL\nlocal-infile=0\n",
            apache_path(&p.directory)?,
            apache_path(&dir)?,
            spec.port,
            apache_path(&self.layout.path(format!("logs/{}-error.log", spec.id))?)?,
            apache_path(&self.layout.path(format!("runtime/state/{}.pid", spec.id))?)?,
            apache_path(&self.layout.path("runtime/temp")?)?
        );
        if p.version.starts_with("8.") {
            text.push_str("mysqlx=0\n");
        }
        if p.version.starts_with("5.7.") {
            text.push_str("log_syslog=0\n");
        }
        service_config::prepare(
            &self.layout,
            &self.service_config_path(&format!("mysql:{}", spec.id))?,
            Kind::Mysql,
            &text,
            "",
            "",
            &out,
            Some(&crate::config_template::load(
                &self.layout,
                &p.directory,
                Kind::Mysql,
                &text,
            )?),
        )?;
        let mut cmd = platform::command(&p.executable, &self.layout.root, &p.directory);
        cmd.arg(format!("--defaults-file={}", out.display()))
            .args(["--verbose", "--help"]);
        if let Err(error) = platform::capture(&mut cmd, Duration::from_secs(10)) {
            let message = format!("{error:#}");
            let diagnostics = message
                .lines()
                .filter(|line| {
                    line.contains("[ERROR]")
                        || line.contains("unknown variable")
                        || line.contains("unknown option")
                })
                .take(8)
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "MySQL 配置校验失败：{}",
                if diagnostics.is_empty() {
                    message.chars().take(1000).collect::<String>()
                } else {
                    diagnostics
                }
            );
        }
        Ok(out)
    }
    fn start_mysql(&mut self, id: &str) -> Result<()> {
        let spec = self
            .config
            .mysql
            .iter()
            .find(|m| m.id == id)
            .context("MySQL 实例不存在")?
            .clone();
        let p = self.package(&spec.package, Kind::Mysql)?.clone();
        let candidate = self.mysql_config(&spec)?;
        self.preflight_database(
            Kind::Mysql,
            &p.executable,
            &p.directory,
            &candidate,
            spec.port,
        )?;
        let conf = self.commit_service_config(&format!("mysql:{id}"), &candidate)?;
        let dir = spec.data_path(&self.layout)?;
        let marker = spec.binding_path(&self.layout)?;
        self.inspect_mysql(&spec)?;
        if marker.exists() {
            ensure!(dir.join("mysql").is_dir(), "数据目录不完整，拒绝重新初始化");
        } else {
            ensure!(
                !dir.exists() || fs::read_dir(&dir)?.next().is_none(),
                "已有非空数据目录没有绑定信息，拒绝初始化"
            );
            fs::create_dir_all(&dir)?;
            let initializing = self
                .layout
                .path(dir.join(crate::mysql_data::INITIALIZING))?;
            atomic_write(
                &initializing,
                &crate::mysql_data::binding_bytes(&p, "initialized")?,
            )?;
            let mut cmd = platform::command(&p.executable, &self.layout.root, &p.directory);
            cmd.arg(format!("--defaults-file={}", conf.display()))
                .arg("--initialize-insecure");
            let log = self.layout.path(format!("logs/{id}-initialize.log"))?;
            let mut child = platform::spawn_logged(&mut cmd, &log)?;
            let process = Process::open(child.id())?;
            let instance = format!("mysql:{id}");
            self.running
                .entry(instance.clone())
                .or_default()
                .push(Running {
                    record: Record {
                        id: instance.clone(),
                        kind: Kind::Mysql,
                        identity: process.identity.clone(),
                        ports: vec![],
                        config: conf.clone(),
                        mysql_dual_loopback: false,
                        listeners: Vec::new(),
                        redis_auth: None,
                    },
                    process,
                });
            self.persist()?;
            let started = Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                ensure!(
                    started.elapsed() < Duration::from_secs(180),
                    "MySQL 初始化仍在运行，已保留进程与数据；请查看初始化日志，不要重新初始化"
                );
                thread::sleep(Duration::from_millis(100));
            };
            self.running.remove(&instance);
            self.persist()?;
            ensure!(
                status.success(),
                "MySQL 初始化失败（{status}），保留数据目录；查看 {}",
                log.display()
            );
            atomic_write(
                &marker,
                &crate::mysql_data::binding_bytes(&p, "initialized")?,
            )?;
            fs::remove_file(&initializing)?;
        }
        self.launch(
            &format!("mysql:{id}"),
            Kind::Mysql,
            &p.executable,
            &p.directory,
            vec![
                format!("--defaults-file={}", conf.display()),
                "--console".into(),
            ],
            vec![spec.port],
            conf,
        )?;
        ensure!(
            self.mysql_greeting(&format!("mysql:{id}"))?
                .starts_with(&p.version),
            "MySQL 实际版本不符"
        );
        Ok(())
    }
    fn redis_config(&self) -> Result<PathBuf> {
        let spec = self.config.redis.as_ref().context("未发现 Redis")?;
        let package = self.package(&spec.package, Kind::Redis)?;
        let out = self.generated()?.join("redis.conf");
        let text = format!(
            "port {}\ndir \"{}\"\ndbfilename dump.rdb\nlogfile \"{}\"\n",
            spec.port,
            apache_path(&self.layout.path("data/redis")?)?,
            apache_path(&self.layout.path("logs/redis-server.log")?)?
        );
        service_config::prepare(
            &self.layout,
            &self.service_config_path("redis")?,
            Kind::Redis,
            &text,
            "",
            "",
            &out,
            Some(&crate::config_template::load(
                &self.layout,
                &package.directory,
                Kind::Redis,
                &text,
            )?),
        )?;
        Ok(out)
    }
    fn start_redis(&mut self) -> Result<()> {
        let spec = self.config.redis.as_ref().context("未发现 Redis")?.clone();
        let p = self.package(&spec.package, Kind::Redis)?.clone();
        let candidate = self.redis_config()?;
        let conf = self.commit_service_config("redis", &candidate)?;
        self.launch(
            "redis",
            Kind::Redis,
            &p.executable,
            &self.layout.root.clone(),
            vec![apache_path(conf.strip_prefix(&self.layout.root)?)?],
            vec![spec.port],
            conf,
        )?;
        let started = Instant::now();
        loop {
            let process = self
                .running
                .get("redis")
                .and_then(|v| v.first())
                .context("Redis 进程记录丢失")?;
            let address = process.control_address()?;
            let password = process
                .record
                .redis_auth
                .as_deref()
                .map(platform::secrets::open)
                .transpose()?
                .unwrap_or_default();
            match protocol::redis_authenticated_at(address, &password, &["PING"]) {
                Ok(response) if response.starts_with("+PONG") => break,
                Err(error)
                    if error.to_string().contains("LOADING")
                        && started.elapsed() < Duration::from_secs(30) =>
                {
                    thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(error),
                _ => bail!("Redis 未就绪"),
            }
        }
        Ok(())
    }
    fn apache_config(&self) -> Result<PathBuf> {
        self.apache_config_named("httpd.conf")
    }
    fn apache_config_named(&self, filename: &str) -> Result<PathBuf> {
        let p = self.package(
            self.config.apache.as_deref().context("未选择 Apache")?,
            Kind::Apache,
        )?;
        let dir = self.generated()?;
        let conf = dir.join(filename);
        let health = dir.join("health.php");
        atomic_write(&health,b"<?php header('Content-Type: application/json'); echo json_encode(['version'=>PHP_VERSION,'ini'=>php_ini_loaded_file(),'root'=>$_SERVER['DOCUMENT_ROOT']]);")?;
        let mut text = format!(
            "ServerRoot \"{}\"\nPidFile \"{}\"\nErrorLog \"{}\"\n",
            apache_path(&p.directory)?,
            apache_path(&self.layout.path("runtime/state/apache.pid")?)?,
            apache_path(&self.layout.path("logs/apache-error.log")?)?
        );
        for (address, port) in self.config.http_listeners()? {
            text.push_str(&format!("Listen {address}:{port}\n"));
        }
        text.push_str(&format!("DefaultRuntimeDir \"{}\"\n", apache_path(&dir)?));
        for module in [
            "authz_core",
            "authz_host",
            "authz_user",
            "authn_core",
            "access_compat",
            "dir",
            "autoindex",
            "mime",
            "log_config",
            "alias",
            "rewrite",
            "env",
            "setenvif",
            "proxy",
            "proxy_fcgi",
            "proxy_balancer",
            "lbmethod_byrequests",
            "slotmem_shm",
        ] {
            let path = self
                .layout
                .path(p.directory.join(format!("modules/mod_{module}.so")))?;
            ensure!(path.is_file(), "Apache 缺少模块：{module}");
            text.push_str(&format!(
                "LoadModule {module}_module \"{}\"\n",
                apache_path(&path)?
            ));
        }
        text.push_str(&format!("TypesConfig \"{}\"\nProxyRequests Off\nProxyFCGIBackendType GENERIC\n<Directory />\nAllowOverride None\nRequire all denied\n</Directory>\n",apache_path(&p.directory.join("conf/mime.types"))?));
        // Winnt proxy_fcgi prepends a slash to drive-qualified filenames; php-cgi requires the drive at the start.
        text.push_str("ProxyFCGISetEnvIf \"reqenv('SCRIPT_FILENAME') =~ m|^/([A-Za-z]:/.*)$|\" SCRIPT_FILENAME \"$1\"\n");
        let extra = fs::read_to_string(self.layout.path("config/overrides/apache.conf")?)?;
        let head = text;
        let text = crate::web::routes(&self.layout, &self.config, &dir, &self.token)?;
        atomic_write(
            &self.layout.path(dir.join("portal/index.html"))?,
            crate::web::navigation_with_packages(&self.config, &self.packages).as_bytes(),
        )?;
        let conf = service_config::prepare(
            &self.layout,
            &self.service_config_path("apache")?,
            Kind::Apache,
            &head,
            &text,
            &format!(
                "ServerName localhost\nLogLevel warn\nThreadsPerChild 32\nDirectoryIndex index.php index.html\n{extra}"
            ),
            &conf,
            Some(&crate::config_template::load(
                &self.layout,
                &p.directory,
                Kind::Apache,
                &head,
            )?),
        )?;
        let mut cmd = platform::command(&p.executable, &self.layout.root, &p.directory);
        cmd.arg("-f").arg(&conf).arg("-t");
        platform::capture(&mut cmd, Duration::from_secs(10)).context("Apache 配置校验失败")?;
        Ok(conf)
    }
    fn start_apache(&mut self) -> Result<()> {
        self.config.apache_dependencies()?;
        let candidate = self.apache_config()?;
        let conf = self.commit_service_config("apache", &candidate)?;
        for id in self.config.apache_dependencies()? {
            self.start(&id)?;
        }
        let p = self
            .package(
                self.config.apache.as_deref().context("未选择 Apache")?,
                Kind::Apache,
            )?
            .clone();
        self.launch(
            "apache",
            Kind::Apache,
            &p.executable,
            &p.directory,
            vec!["-f".into(), conf.to_string_lossy().into_owned()],
            self.config.http_ports(),
            conf,
        )?;
        self.verify_sites()?;
        self.applied = Some(self.config.clone());
        Ok(())
    }
    pub fn verify_sites(&self) -> Result<()> {
        let proc = self
            .running
            .get("apache")
            .and_then(|v| v.first())
            .context("Apache 未运行")?;
        ensure!(
            proc.process.owns_port(self.config.http_port)?,
            "Apache 端口归属不符"
        );
        for port in self.config.http_ports() {
            ensure!(proc.process.owns_port(port)?, "Apache 端口归属不符：{port}");
        }
        let (status, _) = protocol::http(self.config.http_port, "localhost", "/")?;
        ensure!(status == 200, "项目导航页检查失败 HTTP {status}");
        for project in self.config.projects.iter().filter(|p| p.enabled) {
            let root = self.layout.project_root(project)?;
            let php = project
                .php
                .as_ref()
                .or(self.config.default_php.as_ref())
                .context("PHP 未选择")?;
            self.verify_http(
                self.config.http_port,
                "localhost",
                &format!("/_flaragon_{}_{}.php", self.token, project.id),
                php,
                &root,
            )?;
            if project.virtual_host {
                self.verify_http(
                    self.config.http_port,
                    &project.domain,
                    &format!("/_flaragon_{}.php", self.token),
                    php,
                    &root,
                )?;
            }
        }
        for php in self.config.php.iter().filter(|p| p.directory_entry.enabled) {
            self.verify_http(
                php.directory_entry.port,
                "localhost",
                &format!("/_flaragon_{}.php", self.token),
                &php.package,
                &self.layout.site_path(&php.directory_entry.directory)?,
            )?;
        }
        Ok(())
    }
    fn verify_http(
        &self,
        port: u16,
        host: &str,
        route: &str,
        php: &str,
        root: &Path,
    ) -> Result<()> {
        let (status, body) = protocol::http(port, host, route)?;
        ensure!(
            status == 200,
            "入口 {host}:{port}{route} 检查失败 HTTP {status}"
        );
        let json: serde_json::Value = serde_json::from_str(&body).context("PHP 诊断响应无效")?;
        let package = self.package(php, Kind::Php)?;
        ensure!(
            json["version"].as_str() == Some(&package.version),
            "入口 PHP 版本不符：{host}:{port}（预期 {}，实际 {}）",
            package.version,
            json["version"].as_str().unwrap_or("缺少有效版本")
        );
        let expected = apache_path(root)?;
        ensure!(
            json["root"].as_str().is_some_and(|v| v
                .replace('\\', "/")
                .trim_end_matches('/')
                .eq_ignore_ascii_case(expected.trim_end_matches('/'))),
            "入口根目录未生效：{host}:{port}"
        );
        let ini = apache_path(&self.service_config_path(&format!("php:{php}"))?)?;
        ensure!(
            json["ini"]
                .as_str()
                .is_some_and(|v| v.replace('\\', "/").eq_ignore_ascii_case(&ini)),
            "入口 ini 未生效：{host}:{port}"
        );
        Ok(())
    }
    pub fn apply(&mut self, candidate: Config) -> Result<()> {
        self.apply_selected(candidate, None)
    }
    fn apply_selected(&mut self, mut candidate: Config, selected: Option<&str>) -> Result<()> {
        for project in &candidate.projects {
            ensure!(
                project.php.is_some(),
                "项目 {} 必须明确选择 PHP 版本",
                project.label()
            );
        }
        candidate.validate(&self.layout)?;
        self.validate_settings(&candidate)?;
        let old = self.config.clone();
        let mut without_projects = candidate.clone();
        without_projects.projects = old.projects.clone();
        without_projects.revision = old.revision;
        let projects_only = serde_json::to_value(&without_projects)? == serde_json::to_value(&old)?;
        candidate.revision = old.revision.checked_add(1).context("配置版本号超出范围")?;
        let ids = if let Some(id) = selected {
            vec![id.to_owned()]
        } else if projects_only {
            self.service_ids_for(&candidate)
                .into_iter()
                .filter(|id| id == "apache")
                .collect()
        } else {
            self.service_ids_for(&candidate)
        };
        let old_ids = self.running_ids();
        let apache_running = old_ids.iter().any(|id| id == "apache");
        if apache_running {
            candidate.apache_dependencies()?;
        }
        let old_applied = self.applied.clone();
        let old_snapshots = self.applied_files.clone();
        let mut files = BTreeMap::new();
        let mut deferred = BTreeSet::new();
        for id in &ids {
            if self.deferred_config(&candidate, id)? {
                ensure!(
                    !old_ids.contains(id),
                    "缺失组件关联的服务仍在运行，请先停止后修改"
                );
                deferred.insert(id.clone());
                continue;
            }
            for (path, disk) in service_config::snapshot(
                &self.layout,
                &self.service_config_path_for(&candidate, id)?,
            )? {
                let bytes = if old_ids.contains(id) {
                    old_snapshots.get(&path).cloned().unwrap_or(disk)
                } else {
                    disk
                };
                files.insert(path, bytes);
            }
        }
        self.config = candidate;
        let prepared: Result<Vec<_>> = (|| {
            self.ensure_overrides()?;
            ids.iter()
                .filter(|id| !deferred.contains(*id))
                .map(|id| self.service_candidate(id).map(|path| (id.clone(), path)))
                .collect()
        })();
        let prepared = match prepared {
            Ok(value) => value,
            Err(error) => {
                self.config = old;
                return Err(error.context("校验失败，未启停任何服务；请修正后重新保存"));
            }
        };
        let changes: Result<Vec<_>> = prepared
            .iter()
            .filter(|(id, _)| old_ids.contains(id))
            .map(|(id, path)| {
                let active = self.service_config_path(id)?;
                Ok((
                    id.clone(),
                    selected == Some(id.as_str()) || files.get(&active) != Some(&fs::read(path)?),
                ))
            })
            .collect();
        let changes = match changes {
            Ok(changes) => changes,
            Err(error) => {
                self.config = old;
                return Err(error);
            }
        };
        let reload = changes
            .iter()
            .any(|(id, changed)| id == "apache" && *changed);
        let restart: Vec<_> = changes
            .into_iter()
            .filter(|(id, changed)| id != "apache" && *changed)
            .map(|(id, _)| id)
            .collect();
        let mut stopped = Vec::new();
        let mut reload_attempted = false;
        let result: Result<()> = (|| {
            atomic_write(
                &self.layout.path("config/app.backup.json")?,
                &serde_json::to_vec_pretty(&old)?,
            )?;
            for id in &restart {
                stopped.push(id.clone());
                self.stop_internal(id)?;
            }
            for (id, path) in &prepared {
                self.commit_service_config(id, path)?;
            }
            self.layout.save(&self.config)?;
            for id in &restart {
                self.start(id)?;
            }
            if apache_running {
                for id in self.config.apache_dependencies()? {
                    self.start(&id)?;
                }
                if reload {
                    let active = self.service_config_path("apache")?;
                    reload_attempted = true;
                    self.reload_apache_bytes(&active, &fs::read(&active)?)?;
                } else if restart.iter().any(|id| id.starts_with("php:")) {
                    self.verify_sites()?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let mut recovery_errors = Vec::new();
            let cleanup: Vec<_> = self
                .running_ids()
                .into_iter()
                .filter(|id| !old_ids.contains(id) || stopped.contains(id))
                .collect();
            for id in cleanup {
                if let Err(e) = self.stop_internal(&id) {
                    recovery_errors.push(format!("{id} 停止失败：{e:#}"));
                }
            }
            self.config = old;
            for (path, bytes) in &files {
                if let Err(e) = atomic_write(path, bytes) {
                    recovery_errors.push(e.to_string());
                }
            }
            if let Err(e) = self.layout.save(&self.config) {
                recovery_errors.push(e.to_string());
            }
            if recovery_errors.is_empty() {
                for id in &stopped {
                    if let Err(e) = self.start(id) {
                        recovery_errors.push(format!("{id} 恢复失败：{e:#}"));
                    }
                }
                if reload_attempted {
                    let active = self.service_config_path("apache")?;
                    if let Some(bytes) = files.get(&active)
                        && let Err(e) = self.reload_apache_bytes(&active, bytes)
                    {
                        recovery_errors.push(format!("Apache 恢复失败：{e:#}"));
                    }
                }
            }
            self.applied = if recovery_errors.is_empty() {
                old_applied
            } else {
                None
            };
            self.applied_files = old_snapshots;
            self.persist()?;
            bail!(
                "保存应用失败：{error:#}；{}；编辑内容保留，请修正后重新保存",
                if recovery_errors.is_empty() {
                    "上一配置和运行状态已恢复".to_owned()
                } else {
                    recovery_errors.join("；")
                }
            );
        }
        if apache_running && deferred.is_empty() {
            self.applied = Some(self.config.clone());
        } else {
            self.applied = None;
        }
        // Only verified services advance their running snapshots.
        self.applied_files = old_snapshots;
        for (id, _) in &prepared {
            if old_ids.contains(id) {
                self.applied_files.extend(service_config::snapshot(
                    &self.layout,
                    &self.service_config_path(id)?,
                )?);
            }
        }
        for id in self
            .running_ids()
            .into_iter()
            .filter(|id| !old_ids.contains(id))
        {
            self.applied_files.extend(service_config::snapshot(
                &self.layout,
                &self.service_config_path(&id)?,
            )?);
        }
        self.message = if !deferred.is_empty() {
            "设置已保存；缺失组件关联配置尚未校验，其他服务保持运行".into()
        } else if reload || !restart.is_empty() {
            format!(
                "保存并应用成功；{}{}；其他服务保持运行",
                if reload {
                    "Apache 已重加载并验证"
                } else {
                    ""
                },
                if restart.is_empty() {
                    String::new()
                } else {
                    format!(" 已重启并验证：{}", restart.join("、"))
                }
            )
        } else {
            "配置已保存并完成预检查；已停止服务保持停止，启动后验证生效".into()
        };
        self.persist()?;
        Ok(())
    }
}

fn next_port(config: &Config, start: u16, count: u16) -> Result<u16> {
    let mut used: BTreeSet<_> = config.http_ports().into_iter().collect();
    for p in &config.php {
        for offset in 0..p.workers {
            used.insert(p.port.saturating_add(offset));
        }
    }
    used.extend(config.mysql.iter().map(|m| m.port));
    if let Some(r) = &config.redis {
        used.insert(r.port);
    }
    (start..=u16::MAX - count)
        .find(|first| (0..count).all(|offset| !used.contains(&(first + offset))))
        .context("没有可分配的配置端口")
}
