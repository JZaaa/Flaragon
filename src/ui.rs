use crate::{
    Context, Result,
    config::*,
    engine::Snapshot,
    platform::{AppLock, RestoreSignal, wide},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, SyncSender},
    time::Duration,
};
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::*};
mod editor;
mod worker;
use worker::{Command, Event};
slint::include_modules!();

pub fn show_error(message: &str) {
    // SAFETY: The UTF-16 buffers remain live throughout the synchronous modal call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("Flaragon").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
fn confirm(message: &str) -> bool {
    // SAFETY: Both UTF-16 buffers remain live for this synchronous modal dialog.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("请确认").as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}
fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(items)).into()
}
fn restore_window(ui: &MainWindow) -> Result<()> {
    ui.show()?;
    ui.window().set_minimized(false);
    let provider = ui.window().window_handle();
    if let Ok(handle) = provider.window_handle()
        && let RawWindowHandle::Win32(handle) = handle.as_raw()
    {
        // SAFETY: Slint owns this live native window; the provider retains its lifetime.
        unsafe {
            SetForegroundWindow(handle.hwnd.get() as _);
        }
    }
    Ok(())
}
fn label(s: &Snapshot, id: &str) -> String {
    s.packages
        .iter()
        .find(|p| p.id == id)
        .map(|p| format!("{} · {}", p.label(), p.version))
        .unwrap_or_else(|| format!("{id}（缺失）"))
}
struct State {
    layout: Layout,
    tx: SyncSender<Command>,
    editor: editor::Client,
    snapshot: Option<Snapshot>,
    draft: Option<Snapshot>,
    error: String,
    pending: bool,
    applying: bool,
    mysql_generation: u64,
    mysql_dirty: Option<std::time::Instant>,
    choice_target: Option<(String, i32)>,
    mysql_dialog: Option<MysqlAdoptWindow>,
}
impl State {
    fn refresh_locks(&self, ui: &MainWindow) {
        let Some(s) = &self.snapshot else { return };
        let active = |id: &str| s.rows.iter().any(|r| r.id == id && r.running);
        let apache = active("apache");
        ui.set_apache_locked(apache);
        ui.set_redis_locked(active("redis"));
        let dependencies = if apache {
            s.config.apache_dependencies().unwrap_or_default()
        } else {
            vec![]
        };
        let php = ui.get_php_settings();
        for (index, mut row) in php.iter().enumerate() {
            let id = format!("php:{}", row.id);
            let locked = active(&id) || dependencies.contains(&id);
            if row.locked != locked {
                row.locked = locked;
                php.set_row_data(index, row);
            }
        }
        let mysql = ui.get_mysql_settings();
        for (index, mut row) in mysql.iter().enumerate() {
            let locked = !row.fresh && active(&format!("mysql:{}", row.id));
            if row.locked != locked {
                row.locked = locked;
                mysql.set_row_data(index, row);
            }
        }
    }
    fn queue_mysql_inspection(&mut self) {
        self.mysql_generation = self.mysql_generation.wrapping_add(1);
        self.mysql_dirty = Some(std::time::Instant::now());
    }
    fn inspect_mysql_if_ready(&mut self, ui: &MainWindow) {
        if self.pending
            || self
                .mysql_dirty
                .is_none_or(|at| at.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        let Some(s) = &self.draft else { return };
        let rows: Vec<_> = ui
            .get_mysql_settings()
            .iter()
            .map(|r| {
                let package = s
                    .packages
                    .iter()
                    .filter(|p| p.kind == Kind::Mysql)
                    .nth(r.package_index as usize)
                    .map(|p| p.id.clone())
                    .unwrap_or_default();
                (r.name.trim().to_string(), package, r.fresh)
            })
            .collect();
        if !rows.iter().any(|(_, _, fresh)| *fresh) {
            self.mysql_dirty = None;
            return;
        }
        if self
            .tx
            .try_send(Command::InspectMysql(self.mysql_generation, rows))
            .is_ok()
        {
            self.mysql_dirty = None;
        }
    }
    fn send(&mut self, ui: &MainWindow, command: Command) -> Result<()> {
        crate::ensure!(!self.pending, "正在处理上一项操作，请稍候");
        self.applying = matches!(&command, Command::Apply(_) | Command::Settings(_));
        match self.tx.try_send(command) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(_)) => crate::bail!("后台任务队列已满，请稍后重试"),
            Err(mpsc::TrySendError::Disconnected(Command::Exit)) => {
                self.editor.shutdown()?;
                slint::quit_event_loop()?;
                return Ok(());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                crate::bail!("后台已停止，现有服务保持原状；请退出面板后重新启动")
            }
        }
        self.pending = true;
        ui.set_busy(true);
        ui.set_failed(false);
        Ok(())
    }
    fn fail(&mut self, ui: &MainWindow, error: String) {
        ui.set_status(error.clone().into());
        ui.set_failed(true);
        self.error = error;
    }
    fn populate(&mut self, ui: &MainWindow, s: Snapshot, error: Option<String>) {
        let initial = self.snapshot.is_none();
        ui.set_services(model(
            s.rows
                .iter()
                .filter(|r| {
                    s.config
                        .service_usage(&r.id)
                        .is_ok_and(|u| u != ServiceUsage::Disabled)
                })
                .map(|r| {
                    let (kind, package) = if r.id == "apache" {
                        (Kind::Apache, s.config.apache.as_deref())
                    } else if r.id == "redis" {
                        (
                            Kind::Redis,
                            s.config.redis.as_ref().map(|p| p.package.as_str()),
                        )
                    } else if let Some(id) = r.id.strip_prefix("php:") {
                        (Kind::Php, Some(id))
                    } else {
                        (
                            Kind::Mysql,
                            s.config
                                .mysql
                                .iter()
                                .find(|p| r.id == format!("mysql:{}", p.id))
                                .map(|p| p.package.as_str()),
                        )
                    };
                    let p = s.packages.iter().find(|p| Some(p.id.as_str()) == package);
                    let active = r.running;
                    ServiceRow {
                        id: r.id.clone().into(),
                        name: p.map(|p| p.label()).unwrap_or(&r.title).into(),
                        subtitle: format!(
                            "{} · {}{}",
                            kind.label(),
                            p.map(|p| p.version.as_str()).unwrap_or("组件缺失"),
                            r.id.strip_prefix("mysql:")
                                .map(|id| format!(
                                    " · {}",
                                    s.config
                                        .mysql
                                        .iter()
                                        .find(|m| m.id == id)
                                        .map(|m| m.name.as_str())
                                        .unwrap_or(id)
                                ))
                                .unwrap_or_default()
                        )
                        .into(),
                        module: kind.label().into(),
                        common: s
                            .config
                            .service_usage(&r.id)
                            .is_ok_and(|usage| usage == ServiceUsage::Common),
                        state: r.state.clone().into(),
                        ports: r.ports.clone().into(),
                        detail: r.detail.clone().into(),
                        running: active,
                        can_start: r.state != "运行中",
                        can_stop: active,
                    }
                })
                .collect(),
        ));
        ui.set_projects(model(
            s.config
                .projects
                .iter()
                .map(|p| ProjectRow {
                    name: p.label().into(),
                    domain: format!("/{} / · {}", p.id, p.group)
                        .replace(" /", "/")
                        .into(),
                    enabled: p.enabled,
                    shared: p.lan_access,
                    visible: true,
                })
                .collect(),
        ));
        self.filter_projects(ui);
        ui.set_running_count(s.rows.iter().filter(|r| r.state == "运行中").count() as i32);
        ui.set_project_count(s.config.projects.len() as i32);
        ui.set_package_count(s.packages.len() as i32);
        let configuration_status = match s.applied_revision {
            Some(revision) if revision == s.config.revision => "配置已生效",
            Some(_) => "配置已保存，待应用",
            None => "配置已保存，待启动验证",
        };
        ui.set_status(format!("{}  ·  {configuration_status}", s.message).into());
        ui.set_failed(false);
        let counts = ["", "Apache", "PHP", "MySQL", "Redis"]
            .iter()
            .map(|kind| {
                ui.get_services()
                    .iter()
                    .filter(|r| kind.is_empty() || r.module == *kind)
                    .count() as i32
            })
            .collect();
        ui.set_service_counts(model(counts));
        self.snapshot = Some(s);
        self.refresh_locks(ui);
        if initial {
            self.load_draft(ui);
            self.select_project(ui, -1);
        }
        if let Some(error) = error {
            self.fail(ui, error);
        }
    }
    fn load_draft(&mut self, ui: &MainWindow) {
        ui.set_launch_at_login(ui.get_saved_launch_at_login());
        ui.set_start_common_services(ui.get_saved_start_common_services());
        let Some(s) = self.snapshot.as_ref() else {
            return;
        };
        self.draft = Some(s.clone());
        self.mysql_generation = self.mysql_generation.wrapping_add(1);
        self.mysql_dirty = None;
        let c = &s.config;
        ui.set_default_php_options(model(
            c.php.iter().map(|p| label(s, &p.package).into()).collect(),
        ));
        ui.set_php_options(model(
            std::iter::once("使用默认 PHP".into())
                .chain(c.php.iter().map(|p| label(s, &p.package).into()))
                .collect(),
        ));
        let apache: Vec<_> = s
            .packages
            .iter()
            .filter(|p| p.kind == Kind::Apache)
            .collect();
        ui.set_apache_options(model(
            std::iter::once("未选择".into())
                .chain(apache.iter().map(|p| label(s, &p.id).into()))
                .collect(),
        ));
        ui.set_apache_index(
            apache
                .iter()
                .position(|p| Some(&p.id) == c.apache.as_ref())
                .map(|i| i as i32 + 1)
                .unwrap_or(0),
        );
        ui.set_default_php_index(
            c.php
                .iter()
                .position(|p| Some(&p.package) == c.default_php.as_ref())
                .map(|i| i as i32)
                .unwrap_or(-1),
        );
        ui.set_http_port(c.http_port.to_string().into());
        ui.set_http_address(c.http_address.clone().into());
        ui.set_address_options(model(
            s.addresses.iter().map(|a| a.as_str().into()).collect(),
        ));
        ui.set_apache_usage(c.apache_usage.index());
        ui.set_php_settings(model(
            c.php
                .iter()
                .map(|p| PhpSetting {
                    name: label(s, &p.package).into(),
                    id: p.package.clone().into(),
                    port: p.port.to_string().into(),
                    workers: p.workers.to_string().into(),
                    usage: p.usage.index(),
                    locked: false,
                    directory_enabled: p.directory_entry.enabled,
                    directory_port: if p.directory_entry.port == 0 {
                        String::new()
                    } else {
                        p.directory_entry.port.to_string()
                    }
                    .into(),
                    directory_root: p
                        .directory_entry
                        .directory
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    directory_address: p.directory_entry.address.clone().into(),
                })
                .collect(),
        ));
        let mysql: Vec<_> = s
            .packages
            .iter()
            .filter(|p| p.kind == Kind::Mysql)
            .collect();
        ui.set_mysql_package_options(model(
            mysql.iter().map(|p| label(s, &p.id).into()).collect(),
        ));
        ui.set_mysql_settings(model(
            c.mysql
                .iter()
                .map(|p| MysqlSetting {
                    id: p.id.clone().into(),
                    name: p.name.clone().into(),
                    fresh: false,
                    locked: false,
                    hint: self
                        .layout
                        .root
                        .join(if p.series.is_empty() {
                            format!("data/mysql/{}", p.id)
                        } else {
                            format!("data/mysql/{}/{}", p.series, p.name)
                        })
                        .display()
                        .to_string()
                        .into(),
                    exists: false,
                    valid: true,
                    reuse_data: p.reuse_data,
                    can_adopt: false,
                    data_warning: false,
                    package_index: mysql
                        .iter()
                        .position(|a| a.id == p.package)
                        .map(|i| i as i32)
                        .unwrap_or(-1),
                    port: p.port.to_string().into(),
                    usage: p.usage.index(),
                })
                .collect(),
        ));
        ui.set_has_redis(c.redis.is_some());
        ui.set_redis_usage(
            c.redis
                .as_ref()
                .map(|p| p.usage)
                .unwrap_or_default()
                .index(),
        );
        ui.set_redis_port(
            c.redis
                .as_ref()
                .map(|p| p.port)
                .unwrap_or(6379)
                .to_string()
                .into(),
        );
        self.refresh_locks(ui);
    }
    fn select_project(&self, ui: &MainWindow, index: i32) {
        let Some(s) = self.draft.as_ref() else { return };
        let p = usize::try_from(index)
            .ok()
            .and_then(|i| s.config.projects.get(i));
        ui.set_project_index(if p.is_some() { index } else { -1 });
        ui.set_project_name(p.map(|p| p.label()).unwrap_or("").into());
        ui.set_project_id(p.map(|p| p.id.as_str()).unwrap_or("").into());
        ui.set_project_group(p.map(|p| p.group.as_str()).unwrap_or("").into());
        ui.set_project_virtual_host(p.is_some_and(|p| p.virtual_host));
        ui.set_project_lan(p.is_some_and(|p| p.lan_access));
        ui.set_project_listed(p.is_none_or(|p| p.listed));
        ui.set_project_domain(p.map(|p| p.domain.as_str()).unwrap_or("").into());
        ui.set_project_directory(
            p.map(|p| p.directory.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        ui.set_project_docroot(
            p.map(|p| p.document_root.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        ui.set_project_enabled(p.is_none_or(|p| p.enabled));
        ui.set_project_php(
            p.and_then(|p| {
                s.config
                    .php
                    .iter()
                    .position(|a| Some(&a.package) == p.php.as_ref())
            })
            .map(|i| i as i32 + 1)
            .unwrap_or(0),
        );
    }
    fn candidate(&self) -> Result<&Snapshot> {
        let s = self.draft.as_ref().context("环境尚未加载")?;
        crate::ensure!(
            self.snapshot
                .as_ref()
                .is_some_and(|latest| latest.config.revision == s.config.revision),
            "环境配置已变化，请重新进入此页面后编辑"
        );
        Ok(s)
    }
    fn filter_projects(&self, ui: &MainWindow) {
        let query = ui.get_project_search().to_lowercase();
        let filter = ui.get_project_filter();
        let rows = ui.get_projects();
        for (index, mut row) in rows.iter().enumerate() {
            row.visible = format!("{} {}", row.name, row.domain)
                .to_lowercase()
                .contains(&query)
                && (filter == 0 || (filter == 1 && row.shared) || (filter == 2 && !row.shared));
            rows.set_row_data(index, row);
        }
    }
    fn action(&mut self, ui: &MainWindow, name: &str, id: &str) -> Result<()> {
        match name {
            "project-filter" => self.filter_projects(ui),
            "projects-share" | "projects-local" => {
                let mut c = self.candidate()?.config.clone();
                for (i, row) in ui.get_projects().iter().enumerate() {
                    if row.visible {
                        c.projects[i].lan_access = name == "projects-share";
                    }
                }
                self.send(ui, Command::Apply(c))?;
            }
            "service-directory" => {
                crate::ensure!(
                    matches!(id, "apache" | "php" | "mysql" | "redis"),
                    "未知服务目录"
                );
                let path = self.layout.path(format!("bin/{id}"))?;
                crate::ensure!(path.is_dir(), "服务目录不存在：{}", path.display());
                let path = wide(path.as_os_str());
                // SAFETY: The checked environment directory and UTF-16 buffers remain live during the call.
                let result = unsafe {
                    ShellExecuteW(
                        std::ptr::null_mut(),
                        wide("open").as_ptr(),
                        path.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        SW_SHOWNORMAL,
                    )
                };
                crate::ensure!(result as usize > 32, "无法打开服务目录");
            }
            "show" => {
                restore_window(ui)?;
            }
            "settings-reset" => self.load_draft(ui),
            "startup-save" => {
                self.send(
                    ui,
                    Command::Startup(crate::startup::Settings {
                        launch_at_login: ui.get_launch_at_login(),
                        start_common_services: ui.get_start_common_services(),
                    }),
                )?;
            }
            "error-detail" => self.editor.open("诊断", &self.error, true, None)?,
            "exit" => {
                crate::ensure!(!self.pending, "请等待当前操作完成");
                self.editor.check_exit()?;
            }
            "settings-save" => {
                let s = self.candidate()?;
                let mut c = s.config.clone();
                c.apache_usage = ServiceUsage::from_index(ui.get_apache_usage())?;
                c.http_port = ui
                    .get_http_port()
                    .trim()
                    .parse()
                    .context("HTTP 端口必须是数字")?;
                c.http_address = ui.get_http_address().trim().into();
                c.apache = s
                    .packages
                    .iter()
                    .filter(|p| p.kind == Kind::Apache)
                    .nth((ui.get_apache_index() - 1) as usize)
                    .map(|p| p.id.clone());
                c.default_php = c
                    .php
                    .get(ui.get_default_php_index() as usize)
                    .map(|p| p.package.clone());
                c.php = ui
                    .get_php_settings()
                    .iter()
                    .map(|p| {
                        Ok(PhpSpec {
                            package: p.id.to_string(),
                            port: p.port.trim().parse().context("PHP 端口无效")?,
                            workers: p.workers.trim().parse().context("PHP 进程数无效")?,
                            usage: ServiceUsage::from_index(p.usage)?,
                            directory_entry: DirectoryEntry {
                                enabled: p.directory_enabled,
                                port: if p.directory_port.trim().is_empty() && !p.directory_enabled
                                {
                                    0
                                } else {
                                    p.directory_port
                                        .trim()
                                        .parse()
                                        .context("目录入口 HTTP 端口无效")?
                                },
                                directory: p.directory_root.trim().into(),
                                address: p.directory_address.trim().into(),
                            },
                        })
                    })
                    .collect::<Result<_>>()?;
                c.mysql = ui.get_mysql_settings().iter().map(|row| {
                    crate::ensure!(row.valid, "请先检查 MySQL 数据目录：{}", row.hint);
                    let package = s.packages.iter().filter(|p| p.kind == Kind::Mysql).nth(row.package_index as usize).context("请选择有效 MySQL 组件")?;
                    let series = mysql_series(&package.version)?;
                    let name = row.name.trim().to_string();
                    valid_id(&name)?;
                    let mut reuse_data = row.reuse_data;
                    if row.fresh && row.exists && !reuse_data {
                        crate::ensure!(confirm(&format!("{}\n\n确认登记此已有数据目录？已绑定数据会按绑定检查；未绑定数据必须另行“接入数据”才能启动。保存设置不会初始化、升级或降级数据库。", row.hint)), "已取消使用历史数据");
                        reuse_data = true;
                    }
                    Ok(MysqlSpec {
                        id: if row.fresh { format!("{}-{name}", series.replace('.', "-")) } else { row.id.to_string() },
                        name, series, package: package.id.clone(),
                        port: row.port.trim().parse().context("MySQL 端口无效")?,
                        usage: ServiceUsage::from_index(row.usage)?, reuse_data,
                    })
                }).collect::<Result<_>>()?;
                if let Some(r) = &mut c.redis {
                    r.usage = ServiceUsage::from_index(ui.get_redis_usage())?;
                    r.port = ui
                        .get_redis_port()
                        .trim()
                        .parse()
                        .context("Redis 端口无效")?;
                }
                self.send(ui, Command::Settings(c))?;
            }
            "project-save" | "project-remove" => {
                let s = self.candidate()?;
                let mut c = s.config.clone();
                let index = ui.get_project_index();
                if name == "project-remove" {
                    crate::ensure!(
                        index >= 0 && (index as usize) < c.projects.len(),
                        "请选择项目"
                    );
                    c.projects.remove(index as usize);
                } else {
                    let p = Project {
                        id: ui.get_project_id().trim().into(),
                        name: ui.get_project_name().trim().into(),
                        group: ui.get_project_group().trim().into(),
                        virtual_host: ui.get_project_virtual_host(),
                        lan_access: ui.get_project_lan(),
                        listed: ui.get_project_listed(),
                        domain: ui.get_project_domain().trim().into(),
                        directory: PathBuf::from(ui.get_project_directory().trim()),
                        document_root: PathBuf::from(ui.get_project_docroot().trim()),
                        php: c
                            .php
                            .get((ui.get_project_php() - 1) as usize)
                            .map(|p| p.package.clone()),
                        enabled: ui.get_project_enabled(),
                    };
                    if index >= 0 && (index as usize) < c.projects.len() {
                        c.projects[index as usize] = p;
                    } else {
                        c.projects.push(p);
                    }
                }
                self.send(ui, Command::Apply(c))?;
            }
            "project-open" | "project-copy" | "portal-open" | "portal-copy" => {
                let s = self.snapshot.as_ref().context("尚未加载")?;
                let route = if name.starts_with("project-") {
                    let p = s
                        .config
                        .projects
                        .get(ui.get_project_index() as usize)
                        .context("请选择已保存的项目")?;
                    format!("/{}/", p.id)
                } else {
                    "/".into()
                };
                let address = if name.ends_with("open") {
                    "127.0.0.1"
                } else {
                    s.config.http_address.as_str()
                };
                crate::ensure!(
                    address != "0.0.0.0",
                    "复制分享地址前，请在环境设置选择具体的本机 IPv4 地址"
                );
                let url_text = format!("http://{address}:{}{route}", s.config.http_port);
                if name.ends_with("copy") {
                    crate::platform::copy_text(&url_text)?;
                    ui.set_status(format!("已复制：{url_text}").into());
                    return Ok(());
                }
                let url = wide(url_text);
                // SAFETY: The stored, validated address and route form an HTTP URL; buffers span the call.
                let result = unsafe {
                    ShellExecuteW(
                        std::ptr::null_mut(),
                        wide("open").as_ptr(),
                        url.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        SW_SHOWNORMAL,
                    )
                };
                crate::ensure!(result as usize > 32, "无法打开浏览器");
            }
            _ => {
                let c = match name {
                    "scan" => Command::Scan,
                    "start-all" => Command::StartAll,
                    "start-common" => Command::StartCommon,
                    "stop-all" => Command::StopAll,
                    "start" => Command::Start(id.into()),
                    "stop" => Command::Stop(id.into()),
                    "restart" => Command::Restart(id.into()),
                    "reload" => Command::Reload(id.into()),
                    "config" => Command::Read(id.into(), false),
                    "log" => Command::Read(id.into(), true),
                    "packages" => Command::Read("packages".into(), true),
                    "hosts" => Command::Read("hosts".into(), true),
                    "hosts-apply" => Command::Hosts,
                    _ => return Ok(()),
                };
                self.send(ui, c)?;
            }
        }
        Ok(())
    }
}

pub fn run() -> Result<()> {
    let root = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or(
        std::env::current_exe()?
            .parent()
            .context("程序路径无效")?
            .to_path_buf(),
    );
    let layout = Layout::prepare(root)?;
    let _lock = match AppLock::acquire(&layout.root) {
        Ok(lock) => lock,
        Err(e) => {
            if RestoreSignal::request(&layout.root) {
                return Ok(());
            }
            return Err(e);
        }
    };
    let restore = RestoreSignal::new(&layout.root)?;
    layout.initialize()?;
    let (tx, commands) = mpsc::sync_channel(4);
    let (events, rx) = mpsc::sync_channel(4);
    let (editor, editor_events) = editor::start(&layout)?;
    let ui = MainWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_environment(layout.root.display().to_string().into());
    let about = Rc::new(AboutWindow::new()?);
    about.set_app_version(env!("CARGO_PKG_VERSION").into());
    let state = Rc::new(RefCell::new(State {
        layout: layout.clone(),
        tx,
        editor,
        snapshot: None,
        draft: None,
        error: String::new(),
        pending: true,
        applying: false,
        mysql_generation: 0,
        mysql_dirty: None,
        choice_target: None,
        mysql_dialog: None,
    }));
    let weak = ui.as_weak();
    let state_action = state.clone();
    let about_action = about.clone();
    ui.on_action(move |name, id| {
        if name == "about" {
            let _ = about_action.show();
            return;
        }
        if let Some(ui) = weak.upgrade() {
            let mut state = state_action.borrow_mut();
            if let Err(e) = state.action(&ui, &name, &id) {
                state.fail(&ui, format!("{e:#}"));
            }
        }
    });
    let weak = ui.as_weak();
    let nav_state = state.clone();
    ui.on_navigate(move |page| {
        if let Some(ui) = weak.upgrade() {
            let mut s = nav_state.borrow_mut();
            if ui.get_page() != page && !s.pending {
                s.load_draft(&ui);
                s.select_project(&ui, -1);
                ui.set_page(page);
            }
        }
    });
    let weak = ui.as_weak();
    let project_state = state.clone();
    ui.on_select_project(move |index| {
        if let Some(ui) = weak.upgrade() {
            let s = project_state.borrow();
            if !s.pending {
                s.select_project(&ui, index);
            }
        }
    });
    let weak = ui.as_weak();
    let picker_state = state.clone();
    ui.on_choose_directory(move |kind, index| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_busy() {
            return;
        }
        let initial = match kind.as_str() {
            "project" => ui.get_project_directory(),
            "php" if !ui.get_apache_locked() => {
                let Some(row) = ui.get_php_settings().row_data(index as usize) else {
                    return;
                };
                if row.locked {
                    return;
                }
                row.directory_root
            }
            _ => return,
        };
        let provider = ui.window().window_handle();
        let Ok(handle) = provider.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let mut state = picker_state.borrow_mut();
        if let Err(error) = state.send(
            &ui,
            Command::ChooseDirectory(
                kind.to_string(),
                index,
                handle.hwnd.get(),
                PathBuf::from(initial.as_str()),
            ),
        ) {
            state.fail(&ui, format!("{error:#}"));
        }
    });
    let weak = ui.as_weak();
    let picker_state = state.clone();
    ui.on_open_picker(move |kind, index| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_busy() {
            return;
        }
        let (title, options, selected) = match kind.as_str() {
            "http-address" if !ui.get_apache_locked() => {
                ("选择项目入口监听地址", ui.get_address_options(), 0)
            }
            "directory-address" if !ui.get_apache_locked() => {
                ("选择目录入口监听地址", ui.get_address_options(), 0)
            }
            "project" => (
                "选择项目 PHP 版本",
                ui.get_php_options(),
                ui.get_project_php(),
            ),
            "apache" if !ui.get_apache_locked() => (
                "选择 Apache 版本",
                ui.get_apache_options(),
                ui.get_apache_index(),
            ),
            "default-php" if !ui.get_apache_locked() => (
                "选择默认 PHP 版本",
                ui.get_default_php_options(),
                ui.get_default_php_index(),
            ),
            "mysql" => {
                let Some(row) = ui.get_mysql_settings().row_data(index as usize) else {
                    return;
                };
                if row.locked || !row.fresh {
                    return;
                }
                (
                    "选择 MySQL 版本",
                    ui.get_mysql_package_options(),
                    row.package_index,
                )
            }
            _ => return,
        };
        if options.row_count() == 0 {
            return;
        }
        picker_state.borrow_mut().choice_target = Some((kind.to_string(), index));
        ui.set_picker_title(title.into());
        ui.set_picker_options(options);
        ui.set_picker_selected(selected.max(0));
        ui.invoke_show_picker();
    });
    let weak = ui.as_weak();
    let picker_state = state.clone();
    ui.on_pick_option(move |selected| {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_busy() || selected < 0 || selected as usize >= ui.get_picker_options().row_count()
        {
            return;
        }
        let mut state = picker_state.borrow_mut();
        let Some((kind, index)) = state.choice_target.take() else {
            return;
        };
        match kind.as_str() {
            "http-address" if !ui.get_apache_locked() => {
                if let Some(address) = ui.get_address_options().row_data(selected as usize) {
                    ui.set_http_address(address);
                }
            }
            "directory-address" if !ui.get_apache_locked() => {
                let rows = ui.get_php_settings();
                if let (Some(mut row), Some(address)) = (
                    rows.row_data(index as usize),
                    ui.get_address_options().row_data(selected as usize),
                ) && !row.locked
                {
                    row.directory_address = address;
                    rows.set_row_data(index as usize, row);
                }
            }
            "project" => ui.set_project_php(selected),
            "apache" if !ui.get_apache_locked() => ui.set_apache_index(selected),
            "default-php" if !ui.get_apache_locked() => ui.set_default_php_index(selected),
            "mysql" => {
                let rows = ui.get_mysql_settings();
                let Some(mut row) = rows.row_data(index as usize) else {
                    return;
                };
                if row.locked || !row.fresh || row.package_index == selected {
                    return;
                }
                row.package_index = selected;
                row.valid = false;
                row.reuse_data = false;
                rows.set_row_data(index as usize, row);
                state.queue_mysql_inspection();
            }
            _ => {}
        }
    });
    let weak = ui.as_weak();
    let rows_state = state.clone();
    ui.on_settings_row(move |action, index| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_busy() { return; }
            let mut state = rows_state.borrow_mut();
            if action == "directory" || action == "adopt" {
                let Some(row) = ui.get_mysql_settings().row_data(index as usize) else { return };
                if row.fresh || row.locked { return; }
                let result = (|| -> Result<()> {
                    let spec = state.snapshot.as_ref().and_then(|s| s.config.mysql.iter().find(|s| s.id == row.id.as_str())).context("请先保存服务")?;
                    crate::ensure!(row.port.trim() == spec.port.to_string() && row.usage == spec.usage.index(), "请先保存或撤销此服务的设置修改");
                    let command = if action == "directory" { Command::MysqlDirectory(row.id.to_string()) } else { Command::ReviewMysql(row.id.to_string()) };
                    state.send(&ui, command)
                })();
                if let Err(error) = result { state.fail(&ui, format!("{error:#}")); }
                return;
            }
            if action == "inspect" {
                let rows = ui.get_mysql_settings();
                if let Some(mut row) = rows.row_data(index as usize) {
                    if row.locked || !row.fresh { return; }
                    row.valid = false;
                    row.reuse_data = false;
                    // Preserve the repeater and its focused editor while updating validation state.
                    rows.set_row_data(index as usize, row);
                    state.queue_mysql_inspection();
                }
                return;
            }
            let mut rows: Vec<_> = ui.get_mysql_settings().iter().collect();
            if action == "add" {
                let used_ports: Vec<_> = rows.iter().filter_map(|r| r.port.parse::<u16>().ok()).collect();
                let port = (3307..=65535).find(|p| !used_ports.contains(p)).unwrap_or(3307);
                rows.push(MysqlSetting {
                    id: "".into(), name: "".into(), package_index: 0, port: port.to_string().into(),
                    usage: ServiceUsage::Normal.index(), fresh: true, locked: false,
                    hint: "请输入数据目录名称".into(), exists: false, valid: false, reuse_data: false,
                    can_adopt: false, data_warning: false,
                });
            } else if let Some(row) = rows.get(index as usize) {
                if row.locked { return; }
                if action == "remove" {
                    if !row.fresh {
                        let Some(spec) = state.draft.as_ref().and_then(|s| s.config.mysql.iter().find(|m| m.id.as_str() == row.id.as_str())) else { return };
                        let directory = if spec.series.is_empty() {
                            format!("{}\\data\\mysql\\{}", ui.get_environment(), spec.id)
                        } else {
                            format!("{}\\data\\mysql\\{}\\{}", ui.get_environment(), spec.series, spec.name)
                        };
                        if !confirm(&format!("确认移除此 MySQL 服务？\n数据不会删除。数据位置：{directory}\n如需删除数据，请自行前往该目录删除。")) { return; }
                    }
                    rows.remove(index as usize);
                }
            }
            ui.set_mysql_settings(model(rows));
            state.queue_mysql_inspection();
        }
    });
    ui.window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    let tray = Tray::new()?;
    let weak = ui.as_weak();
    tray.on_action(move |name| {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_action(name, "".into());
        }
    });
    tray.show()?;
    std::thread::spawn(move || {
        let failure = events.clone();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker::worker(layout, commands, events)
        }))
        .is_err()
        {
            let _ = failure.send(Event::Fatal(
                "后台意外停止，现有服务保持原状；请退出面板后重新启动".into(),
            ));
        }
    });
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let timer_state = state.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = timer_state.borrow_mut();
            if restore.requested() {
                let _ = restore_window(&ui);
            }
            s.inspect_mysql_if_ready(&ui);
            for event in rx.try_iter().take(8) {
                match event {
                    Event::MysqlDirectory(id, result) => {
                        s.pending = false;
                        ui.set_busy(false);
                        let rows = ui.get_mysql_settings();
                        for (index, mut row) in rows.iter().enumerate() {
                            if row.id.as_str() != id {
                                continue;
                            }
                            match &result {
                                Ok(report) => {
                                    row.hint =
                                        format!("{} · {}", report.path.display(), report.message)
                                            .into();
                                    row.can_adopt = report.can_adopt;
                                    row.data_warning = !matches!(
                                        report.state,
                                        crate::mysql_data::State::Missing
                                            | crate::mysql_data::State::Empty
                                            | crate::mysql_data::State::Bound
                                    );
                                }
                                Err(error) => {
                                    row.hint = error.clone().into();
                                    row.can_adopt = false;
                                    row.data_warning = true;
                                }
                            }
                            rows.set_row_data(index, row);
                        }
                    }
                    Event::MysqlReview(result) => {
                        s.pending = false;
                        ui.set_busy(false);
                        let result = result.map_err(|error| crate::anyhow!(error)).and_then(
                            |review| -> Result<()> {
                                let dialog = MysqlAdoptWindow::new()?;
                                dialog.set_description(review.description.into());
                                dialog.set_expected_version(review.version.into());
                                let weak_dialog = dialog.as_weak();
                                dialog.on_cancel(move || {
                                    if let Some(dialog) = weak_dialog.upgrade() {
                                        let _ = dialog.hide();
                                    }
                                });
                                let weak_dialog = dialog.as_weak();
                                let weak_main = ui.as_weak();
                                let weak_state = Rc::downgrade(&timer_state);
                                dialog.on_submit(move || {
                                    let (Some(dialog), Some(ui), Some(state)) = (
                                        weak_dialog.upgrade(),
                                        weak_main.upgrade(),
                                        weak_state.upgrade(),
                                    ) else {
                                        return;
                                    };
                                    let confirmation = crate::engine::MysqlAdoptionConfirmation {
                                        token: review.token.clone(),
                                        source_version: dialog.get_source_version().to_string(),
                                        source_stopped: dialog.get_source_stopped(),
                                        backup_confirmed: dialog.get_backup_confirmed(),
                                        config_confirmed: dialog.get_config_confirmed(),
                                    };
                                    let _ = dialog.hide();
                                    let mut state = state.borrow_mut();
                                    if let Err(error) =
                                        state.send(&ui, Command::AdoptMysql(confirmation))
                                    {
                                        state.fail(&ui, format!("{error:#}"));
                                    }
                                });
                                if let Some(old) = s.mysql_dialog.take() {
                                    let _ = old.hide();
                                }
                                dialog.show()?;
                                s.mysql_dialog = Some(dialog);
                                Ok(())
                            },
                        );
                        if let Err(error) = result {
                            s.fail(&ui, format!("{error:#}"));
                        }
                    }
                    Event::Startup(result, completed) => {
                        if completed {
                            s.pending = false;
                            ui.set_busy(false);
                        }
                        match result {
                            Ok(report) => {
                                ui.set_saved_launch_at_login(report.settings.launch_at_login);
                                ui.set_saved_start_common_services(
                                    report.settings.start_common_services,
                                );
                                ui.set_launch_at_login(report.settings.launch_at_login);
                                ui.set_start_common_services(report.settings.start_common_services);
                                ui.set_startup_status(report.message.into());
                                ui.set_startup_failed(false);
                            }
                            Err(error) => {
                                ui.set_startup_status(error.clone().into());
                                ui.set_startup_failed(true);
                                s.fail(&ui, error);
                            }
                        }
                    }
                    Event::DirectoryChosen(kind, index, result) => {
                        s.pending = false;
                        ui.set_busy(false);
                        match result {
                            Ok(Some(path)) => {
                                let path = path.to_string_lossy().to_string();
                                if kind == "project" {
                                    ui.set_project_directory(path.into());
                                } else if !ui.get_apache_locked() {
                                    let rows = ui.get_php_settings();
                                    if let Some(mut row) = rows.row_data(index as usize)
                                        && !row.locked
                                    {
                                        row.directory_root = path.into();
                                        rows.set_row_data(index as usize, row);
                                    }
                                }
                                ui.set_status("目录已选择，保存后应用".into());
                            }
                            Ok(None) => ui.set_status("已取消选择，目录保持不变".into()),
                            Err(error) => s.fail(&ui, error),
                        }
                    }
                    Event::MysqlHints(generation, hints) => {
                        if generation == s.mysql_generation {
                            let rows = ui.get_mysql_settings();
                            if rows.row_count() == hints.len() {
                                for (index, hint) in hints.into_iter().enumerate() {
                                    let Some((hint, exists, valid)) = hint else {
                                        continue;
                                    };
                                    let Some(mut row) = rows.row_data(index) else {
                                        continue;
                                    };
                                    row.hint = hint.into();
                                    row.exists = exists;
                                    row.valid = valid;
                                    rows.set_row_data(index, row);
                                }
                            }
                        }
                    }
                    Event::Snapshot(snapshot, error, completed) => {
                        let applied = completed && s.applying;
                        if completed {
                            s.pending = false;
                            ui.set_busy(false);
                        }
                        s.populate(&ui, *snapshot, error.clone());
                        if applied && error.is_none() && ui.get_page() != 0 {
                            let index = ui.get_project_index();
                            s.load_draft(&ui);
                            s.select_project(&ui, index);
                        }
                    }
                    Event::Text(id, text, log, path) => {
                        if let Err(e) = s.editor.open(&id, &text, log, path.as_deref()) {
                            s.fail(&ui, format!("{e:#}"));
                        }
                    }
                    Event::Saved(result) => {
                        let _ = s.editor.saved(result);
                    }
                    Event::Fatal(error) => {
                        s.pending = false;
                        ui.set_busy(false);
                        s.fail(&ui, error);
                    }
                    Event::Exit => {
                        let _ = s.editor.shutdown();
                        let _ = slint::quit_event_loop();
                    }
                }
            }
            for event in editor_events.try_iter().take(4) {
                match event {
                    editor::Event::Save(id, text, baseline) => {
                        if let Err(e) = s.send(&ui, Command::SaveConfig(id, text, baseline)) {
                            let _ = s.editor.saved(Err(format!("{e:#}")));
                        }
                    }
                    editor::Event::ExitReady => {
                        if let Err(e) = s.send(&ui, Command::Exit) {
                            s.fail(&ui, format!("{e:#}"));
                        }
                    }
                    editor::Event::Error(error) => s.fail(&ui, error),
                    editor::Event::HostsApply => {
                        if let Err(e) = s.send(&ui, Command::Hosts) {
                            s.fail(&ui, format!("{e:#}"));
                        }
                    }
                }
            }
        },
    );
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}
