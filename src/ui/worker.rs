use crate::{
    Context, Result,
    config::*,
    engine::{Engine, Snapshot},
};
use std::{
    fs,
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};
pub(super) enum Command {
    Scan,
    StartAll,
    StartCommon,
    StopAll,
    Start(String),
    Stop(String),
    Restart(String),
    Reload(String),
    RemoveMissing(String),
    SwitchPhp(String, String, bool),
    Apply(Config),
    Settings(Config),
    Startup(crate::startup::Settings),
    InspectMysql(u64, Vec<(String, String, bool)>),
    MysqlDirectory(String),
    ReviewMysql(String),
    AdoptMysql(crate::engine::MysqlAdoptionConfirmation),
    ChooseDirectory(String, i32, isize, std::path::PathBuf),
    SaveConfig(String, String, String),
    Read(String, bool),
    Hosts,
    Exit,
}
pub(super) enum Event {
    Startup(std::result::Result<crate::startup::Report, String>, bool),
    DirectoryChosen(
        String,
        i32,
        std::result::Result<Option<std::path::PathBuf>, String>,
    ),
    MysqlHints(u64, Vec<Option<(String, bool, bool)>>),
    MysqlDirectory(
        String,
        std::result::Result<crate::mysql_data::Report, String>,
    ),
    MysqlReview(std::result::Result<crate::engine::MysqlAdoptionReview, String>),
    Saved(std::result::Result<String, String>),
    Snapshot(Box<Snapshot>, Option<String>, bool),
    Text(String, String, bool, Option<String>),
    Fatal(String),
    Exit,
}
pub(super) fn worker(layout: Layout, rx: Receiver<Command>, tx: SyncSender<Event>) {
    let send = |event| {
        let _ = tx.send(event);
    };
    if std::env::var_os("FLARAGON_UI_PROFILE").is_some() {
        // SAFETY: Reads this worker's thread ID for optional project-local diagnostics.
        let id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        if let Ok(path) = layout.path("runtime/state/worker-thread.txt") {
            let _ = atomic_write(&path, id.to_string().as_bytes());
        }
    }
    let mut engine = loop {
        match Engine::new(layout.clone()) {
            Ok(engine) => break engine,
            Err(e) => {
                send(Event::Fatal(format!(
                    "启动检查失败：{e:#}。可刷新重试或退出面板；现有服务保持原状。"
                )));
                loop {
                    match rx.recv() {
                        Ok(Command::Scan) => break,
                        Ok(Command::Exit) => {
                            send(Event::Exit);
                            return;
                        }
                        Ok(_) => send(Event::Fatal("启动检查未通过，请刷新重试或退出面板".into())),
                        Err(_) => return,
                    }
                }
            }
        }
    };
    let path_status = engine.message.clone();
    let mut error = engine.scan().err().map(|e| format!("{e:#}"));
    engine.message = format!("{path_status}；{}", engine.message);
    let startup = crate::startup::inspect(&engine.layout);
    if error.is_none() {
        match crate::startup::settings(&engine.layout) {
            Ok(settings) => {
                if settings.start_common_services {
                    engine.message = "正在自动启动常用服务…".into();
                    send(Event::Snapshot(Box::new(engine.snapshot()), None, false));
                }
                error = crate::startup::start_services(&mut engine, settings)
                    .err()
                    .map(|e| format!("{e:#}"));
            }
            Err(e) => error = Some(format!("启动设置读取失败：{e:#}")),
        }
    }
    send(Event::Startup(startup.map_err(|e| format!("{e:#}")), false));
    send(Event::Snapshot(Box::new(engine.snapshot()), error, true));
    let watch = crate::platform::DirectoryWatch::new(&engine.layout.root.join("bin")).ok();
    let mut changed = None;
    let mut last_state = serde_json::to_string(&engine.snapshot()).unwrap_or_default();
    let mut last_health = std::time::Instant::now();
    loop {
        let command = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if watch.as_ref().is_some_and(|w| w.changed().unwrap_or(false)) {
                    changed = Some(std::time::Instant::now());
                }
                if changed
                    .is_some_and(|time: std::time::Instant| time.elapsed() > Duration::from_secs(2))
                {
                    changed = None;
                    if let Err(error) = engine.scan() {
                        engine.message = format!("自动发现失败：{error:#}");
                    }
                }
                if last_health.elapsed() >= Duration::from_secs(3) {
                    last_health = std::time::Instant::now();
                    let snapshot = engine.snapshot();
                    let state = serde_json::to_string(&snapshot).unwrap_or_default();
                    if state != last_state
                        && tx
                            .try_send(Event::Snapshot(Box::new(snapshot), None, false))
                            .is_ok()
                    {
                        last_state = state;
                    }
                }
                continue;
            }
            Err(_) => break,
        };
        if let Command::InspectMysql(generation, rows) = &command {
            let keys: Vec<_> = rows
                .iter()
                .map(|(name, package, _)| {
                    let series = engine
                        .packages
                        .iter()
                        .find(|p| &p.id == package)
                        .and_then(|p| mysql_series(&p.version).ok())
                        .unwrap_or_default();
                    (series, name.to_lowercase())
                })
                .collect();
            let hints = rows
                .iter()
                .enumerate()
                .map(|(index, (name, package, fresh))| {
                    if !fresh {
                        return None;
                    }
                    let result = (|| -> Result<(String, bool, bool)> {
                        let p = engine
                            .packages
                            .iter()
                            .find(|p| &p.id == package)
                            .context("请选择 MySQL 版本")?;
                        let spec = MysqlSpec {
                            id: String::new(),
                            name: name.clone(),
                            series: mysql_series(&p.version)?,
                            package: package.clone(),
                            port: 3306,
                            usage: ServiceUsage::Normal,
                            reuse_data: false,
                        };
                        let path = spec.data_path(&engine.layout)?;
                        crate::ensure!(
                            keys.iter().filter(|key| *key == &keys[index]).count() == 1,
                            "数据目录名称重复，已被另一项服务使用"
                        );
                        let report = engine.mysql_directory_report(&spec)?;
                        let exists = if report.can_adopt {
                            true
                        } else {
                            engine.inspect_mysql(&spec)?
                        };
                        Ok((
                            format!("{} · {}", path.display(), report.message),
                            exists,
                            true,
                        ))
                    })();
                    Some(result.unwrap_or_else(|e| (format!("{e:#}"), false, false)))
                })
                .collect();
            send(Event::MysqlHints(*generation, hints));
            continue;
        }
        let result = match command {
            Command::MysqlDirectory(id) => {
                let result = engine
                    .config
                    .mysql
                    .iter()
                    .find(|s| s.id == id)
                    .context("请先保存 MySQL 服务")
                    .and_then(|spec| engine.mysql_directory_report(spec));
                send(Event::MysqlDirectory(
                    id,
                    result.map_err(|e| format!("{e:#}")),
                ));
                continue;
            }
            Command::ReviewMysql(id) => {
                send(Event::MysqlReview(
                    engine
                        .review_mysql_adoption(&id)
                        .map_err(|e| format!("{e:#}")),
                ));
                continue;
            }
            Command::AdoptMysql(confirmation) => engine.adopt_mysql(confirmation).map(|spec| {
                send(Event::MysqlDirectory(
                    spec.id.clone(),
                    engine
                        .mysql_directory_report(&spec)
                        .map_err(|e| format!("{e:#}")),
                ));
            }),
            Command::Startup(settings) => {
                let result = crate::startup::apply(&engine.layout, settings)
                    .map_err(|error| format!("{error:#}"));
                send(Event::Startup(result, true));
                continue;
            }
            Command::ChooseDirectory(kind, index, owner, initial) => {
                let result = crate::platform::choose_directory(&engine.layout, owner, &initial)
                    .map_err(|error| format!("{error:#}"));
                send(Event::DirectoryChosen(kind, index, result));
                continue;
            }
            Command::Scan => engine.scan(),
            Command::Hosts => (|| -> Result<()> {
                let nonce = crate::platform::random_token()?;
                crate::platform::elevate_hosts(
                    &std::env::current_exe()?,
                    &engine.layout.root,
                    &nonce,
                )?;
                let result: serde_json::Value = serde_json::from_slice(
                    &fs::read(
                        engine
                            .layout
                            .path(format!("runtime/state/hosts-{nonce}.json"))?,
                    )
                    .context("未取得 hosts 助手结果")?,
                )?;
                crate::ensure!(
                    result["ok"].as_bool() == Some(true),
                    "{}",
                    result["error"].as_str().unwrap_or("hosts 应用失败")
                );
                engine.message = "hosts 映射已应用并验证".into();
                Ok(())
            })(),
            Command::StartAll => engine.start_all(),
            Command::StartCommon => engine.start_common(),
            Command::StopAll => engine.stop_all(),
            Command::Start(id) => engine.start(&id),
            Command::Stop(id) => engine.stop(&id),
            Command::Restart(id) => engine.restart(&id),
            Command::Reload(id) => engine.reload(&id),
            Command::RemoveMissing(id) => engine.remove_missing_service(&id),
            Command::SwitchPhp(source, target, remove) => {
                engine.switch_project_php(&source, &target, remove)
            }
            Command::Apply(config) => engine.apply(config),
            Command::Settings(config) => engine.apply_settings(config),
            Command::InspectMysql(..) => unreachable!(),
            Command::SaveConfig(id, text, baseline) => {
                let result = (|| -> Result<()> {
                    crate::ensure!(
                        engine.read_service_config(&id)? == baseline,
                        "磁盘配置已被其他操作修改。请保留当前编辑内容，重新打开最新配置后合并。"
                    );
                    engine.save_service_config(&id, &text)
                })();
                send(Event::Saved(match &result {
                    Ok(()) => engine
                        .read_service_config(&id)
                        .map_err(|e| format!("{e:#}")),
                    Err(e) => Err(format!("{e:#}")),
                }));
                result
            }
            Command::Read(id, log) => {
                let text = read_text(&engine, &id, log);
                let path = if !log {
                    engine
                        .service_config_path(&id)
                        .ok()
                        .map(|p| p.display().to_string())
                } else {
                    None
                };
                match text {
                    Ok(text) => send(Event::Text(id, text, log, path)),
                    Err(e) => send(Event::Text(id, format!("{e:#}"), true, None)),
                };
                Ok(())
            }
            Command::Exit => match engine.stop_all() {
                Ok(()) => {
                    send(Event::Exit);
                    break;
                }
                Err(e) => Err(e),
            },
        };
        send(Event::Snapshot(
            Box::new(engine.snapshot()),
            result.err().map(|e| format!("{e:#}")),
            true,
        ));
        last_state = serde_json::to_string(&engine.snapshot()).unwrap_or_default();
    }
}
fn read_text(engine: &Engine, id: &str, log: bool) -> Result<String> {
    if id == "hosts" {
        return crate::hosts::preview(&engine.layout, &engine.config);
    }
    if id == "packages" {
        return Ok(engine
            .packages
            .iter()
            .map(|p| {
                format!(
                    "{} {} · {}{}\r\n目录：{}\r\n状态：{}\r\n",
                    p.kind.label(),
                    p.version,
                    p.architecture,
                    p.php_thread_safe
                        .map(|ts| if ts { " TS" } else { " NTS" })
                        .unwrap_or(""),
                    p.directory.display(),
                    p.error.as_deref().unwrap_or("可用")
                )
            })
            .collect::<Vec<_>>()
            .join("\r\n"));
    }
    if id == "settings" {
        return Ok(serde_json::to_string_pretty(&engine.config)?);
    }
    if !log {
        return engine.read_service_config(id);
    }
    let path = if id == "apache" {
        engine.layout.path("logs/apache-error.log")?
    } else if id == "redis" {
        engine.layout.path("logs/redis-server.log")?
    } else if let Some(id) = id.strip_prefix("mysql:") {
        engine.layout.path(format!("logs/{id}-error.log"))?
    } else {
        let error_log = engine
            .layout
            .path(format!("logs/{}-error.log", id.replace(':', "-")))?;
        if error_log.is_file() {
            error_log
        } else {
            engine
                .layout
                .path(format!("logs/{}.log", id.replace(':', "-")))?
        }
    };
    use std::io::{Read, Seek, SeekFrom};
    let mut file =
        fs::File::open(&path).with_context(|| format!("日志尚未生成：{}", path.display()))?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(65536)))?;
    let mut bytes = vec![];
    file.take(65536).read_to_end(&mut bytes)?;
    Ok(if bytes.is_empty() {
        "日志尚无内容。".into()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    })
}
