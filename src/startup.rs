use crate::{
    Context, Result,
    config::{Layout, atomic_write},
    engine::Engine,
    ensure,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub launch_at_login: bool,
    pub start_common_services: bool,
}

pub struct Report {
    pub settings: Settings,
    pub registered: bool,
    pub simulated: bool,
    pub message: String,
}

trait Store {
    fn read(&mut self) -> Result<Option<String>>;
    fn write(&mut self, command: Option<&str>) -> Result<()>;
}

enum RegistrationStore {
    Windows(String),
    Fixture(PathBuf),
}
impl Store for RegistrationStore {
    fn read(&mut self) -> Result<Option<String>> {
        match self {
            Self::Windows(name) => crate::platform::startup::read(name),
            Self::Fixture(path) => {
                crate::config::reject_links(path)?;
                match fs::read(&*path) {
                    Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }
    fn write(&mut self, command: Option<&str>) -> Result<()> {
        match self {
            Self::Windows(name) => crate::platform::startup::write(name, command),
            Self::Fixture(path) => atomic_write(path, &serde_json::to_vec(&command)?),
        }
    }
}

fn store(layout: &Layout) -> Result<RegistrationStore> {
    if layout.development_root().is_some() {
        Ok(RegistrationStore::Fixture(
            layout.path("runtime/state/startup-registration.json")?,
        ))
    } else {
        // Registry value names can contain path separators; the full root avoids environment collisions.
        Ok(RegistrationStore::Windows(format!(
            "Flaragon ({})",
            layout.root.display()
        )))
    }
}

fn command(layout: &Layout) -> Result<String> {
    let path = layout.path("Flaragon.exe")?;
    let path = path.to_str().context("启动路径编码无效")?;
    ensure!(
        !path.contains(['\0', '"', '\r', '\n']),
        "启动路径包含无效字符"
    );
    // The executable derives the environment from its own directory, independent of the login working directory.
    Ok(format!("\"{path}\""))
}

pub(crate) fn settings(layout: &Layout) -> Result<Settings> {
    let path = layout.path("config/startup.json")?;
    let settings: Settings = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("无法读取启动设置")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => return Err(e).context("无法读取启动设置"),
    };
    Ok(settings)
}

pub fn inspect(layout: &Layout) -> Result<Report> {
    let settings = settings(layout)?;
    let actual = store(layout)?.read()?;
    let registered = actual.as_deref() == Some(command(layout)?.as_str());
    ensure!(
        actual.is_none() || registered,
        "本环境的登录启动项已被其他内容占用，请检查系统启动设置"
    );
    let simulated = layout.development_root().is_some();
    let state = match (settings.launch_at_login, registered) {
        (true, true) => "已登记登录启动",
        (false, false) => "软件自启已关闭",
        _ => "启动设置与登记状态不一致，请重新保存启动设置",
    };
    Ok(Report {
        settings,
        registered,
        simulated,
        message: if simulated {
            String::new()
        } else {
            state.into()
        },
    })
}

fn apply_with(layout: &Layout, desired: Settings, store: &mut impl Store) -> Result<()> {
    let command = command(layout)?;
    if desired.launch_at_login {
        ensure!(
            layout.path("Flaragon.exe")?.is_file(),
            "请先将 Flaragon.exe 放在环境根目录，再启用软件自启"
        );
        ensure!(
            command.encode_utf16().count() <= 260,
            "环境路径过长，Windows 登录启动命令不能超过 260 个字符"
        );
    }
    let file = layout.path("config/startup.json")?;
    let bytes = serde_json::to_vec_pretty(&desired)?;
    let previous = store.read()?;
    ensure!(
        previous.as_deref().is_none_or(|s| s == command),
        "本环境的登录启动项已被其他内容占用，未覆盖或删除，请检查系统启动设置"
    );
    let next = desired.launch_at_login.then_some(command.as_str());
    let update = (|| -> Result<()> {
        store.write(next)?;
        ensure!(store.read()?.as_deref() == next, "登录启动项写入后校验失败");
        atomic_write(&file, &bytes).context("无法保存启动设置")
    })();
    if let Err(error) = update {
        let restored = (|| -> Result<()> {
            let current = store.read()?;
            ensure!(
                current.as_deref() == next || current == previous,
                "登录启动项被外部修改，未覆盖"
            );
            store.write(previous.as_deref())?;
            ensure!(store.read()? == previous, "恢复后校验失败");
            Ok(())
        })();
        return Err(crate::anyhow!(
            "{error:#}；登录启动项恢复结果：{}",
            match restored {
                Ok(()) => "已恢复".into(),
                Err(e) => format!("失败：{e:#}"),
            }
        ));
    }
    Ok(())
}

pub fn apply(layout: &Layout, desired: Settings) -> Result<Report> {
    apply_with(layout, desired, &mut store(layout)?)?;
    inspect(layout)
}

/// Called once after the desktop manager's initial scan, never on refresh or tray restoration.
pub fn start_services(engine: &mut Engine, settings: Settings) -> Result<()> {
    if settings.start_common_services {
        engine.start_common().context("常用服务自启失败")?;
        engine.message = "常用服务已自动启动并验证".into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Layout {
        let layout = Layout::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "testwork/startup-space path-{}",
            crate::platform::random_token().unwrap()
        )))
        .unwrap();
        assert!(
            layout.development_root().is_some(),
            "Tests require scripts/env.ps1"
        );
        fs::write(
            layout.path("Flaragon.exe").unwrap(),
            b"non-executable test fixture",
        )
        .unwrap();
        layout
    }

    #[derive(Default)]
    struct FakeStore {
        value: Option<String>,
        fail_next_write: bool,
        replace_next_write: bool,
        writes: usize,
    }
    impl Store for FakeStore {
        fn read(&mut self) -> Result<Option<String>> {
            Ok(self.value.clone())
        }
        fn write(&mut self, value: Option<&str>) -> Result<()> {
            self.writes += 1;
            if std::mem::take(&mut self.fail_next_write) {
                crate::bail!("injected access denial");
            }
            self.value = if std::mem::take(&mut self.replace_next_write) {
                Some("foreign registration".into())
            } else {
                value.map(str::to_owned)
            };
            Ok(())
        }
    }

    const ENABLED: Settings = Settings {
        launch_at_login: true,
        start_common_services: true,
    };

    #[test]
    fn defaults_and_independent_options_are_persisted() {
        let layout = fixture();
        assert_eq!(inspect(&layout).unwrap().settings, Settings::default());
        let mut store = FakeStore::default();
        for launch_at_login in [true, false] {
            for start_common_services in [true, false] {
                let desired = Settings {
                    launch_at_login,
                    start_common_services,
                };
                apply_with(&layout, desired, &mut store).unwrap();
                assert_eq!(settings(&layout).unwrap(), desired);
                assert_eq!(store.value.is_some(), launch_at_login);
            }
        }
    }

    #[test]
    fn simulated_settings_round_trip_without_system_registration() {
        let layout = fixture();
        let report = apply(&layout, ENABLED).unwrap();
        assert!(report.simulated && report.registered);
        assert_eq!(settings(&layout).unwrap(), ENABLED);
        let other = fixture();
        assert!(!inspect(&other).unwrap().registered);
        let cmd = command(&layout).unwrap();
        assert_eq!(cmd, format!("\"{}\\Flaragon.exe\"", layout.root.display()));
        assert!(cmd.contains("space path"));
        let report = apply(&layout, Settings::default()).unwrap();
        assert!(!report.registered && !report.settings.start_common_services);
        assert!(crate::platform::startup::read("Flaragon test").is_err());
        assert!(crate::platform::startup::write("Flaragon test", None).is_err());
    }

    #[test]
    fn failed_preferences_save_restores_existing_registration() {
        let layout = fixture();
        let mut store = FakeStore::default();
        apply_with(&layout, ENABLED, &mut store).unwrap();
        let original = store.value.clone();
        fs::create_dir(layout.path("config/startup.pending").unwrap()).unwrap();
        let error = apply_with(&layout, Settings::default(), &mut store)
            .unwrap_err()
            .to_string();
        assert!(error.contains("已恢复"));
        assert_eq!(store.value, original);
        assert_eq!(settings(&layout).unwrap(), ENABLED);
    }

    #[test]
    fn registration_failure_does_not_commit_preferences() {
        let layout = fixture();
        let mut store = FakeStore {
            fail_next_write: true,
            ..Default::default()
        };
        assert!(apply_with(&layout, ENABLED, &mut store).is_err());
        assert_eq!(store.value, None);
        assert_eq!(settings(&layout).unwrap(), Settings::default());
    }

    #[test]
    fn foreign_registration_is_never_overwritten_or_rolled_back() {
        let layout = fixture();
        let mut store = FakeStore {
            value: Some("foreign registration".into()),
            ..Default::default()
        };
        assert!(apply_with(&layout, Settings::default(), &mut store).is_err());
        assert_eq!(store.writes, 0);
        store.value = None;
        store.replace_next_write = true;
        let error = apply_with(&layout, ENABLED, &mut store)
            .unwrap_err()
            .to_string();
        assert!(error.contains("恢复结果：失败"));
        assert_eq!(store.value.as_deref(), Some("foreign registration"));
        assert_eq!(store.writes, 1);
        assert!(!layout.path("config/startup.json").unwrap().exists());
    }

    #[test]
    fn service_autostart_is_independent_of_login_registration() {
        let layout = fixture();
        let mut engine = Engine::new(layout.clone()).unwrap();
        // A damaged login registration must not prevent reading the independent service option.
        fs::write(
            layout
                .path("runtime/state/startup-registration.json")
                .unwrap(),
            b"broken",
        )
        .unwrap();
        for launch_at_login in [false, true] {
            for start_common_services in [false, true] {
                let desired = Settings {
                    launch_at_login,
                    start_common_services,
                };
                atomic_write(
                    &layout.path("config/startup.json").unwrap(),
                    &serde_json::to_vec(&desired).unwrap(),
                )
                .unwrap();
                assert!(inspect(&layout).is_err());
                engine.message = "untouched".into();
                start_services(&mut engine, settings(&layout).unwrap()).unwrap();
                assert_eq!(
                    engine.message == "常用服务已自动启动并验证",
                    start_common_services
                );
            }
        }
    }

    #[test]
    fn missing_executable_and_corrupt_preferences_do_not_enable_startup() {
        let layout = fixture();
        fs::remove_file(layout.path("Flaragon.exe").unwrap()).unwrap();
        let mut store = FakeStore::default();
        assert!(apply_with(&layout, ENABLED, &mut store).is_err());
        assert_eq!(store.writes, 0);
        fs::write(layout.path("config/startup.json").unwrap(), b"{broken").unwrap();
        assert!(inspect(&layout).is_err());
    }
}
