#![windows_subsystem = "windows"]
use flaragon::{
    Context, Result,
    deployment::{self, Mode, Package},
};
use slint::ComponentHandle;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

slint::slint! {
    import { Button, LineEdit, ScrollView, AboutSlint } from "std-widgets.slint";
    export component DeployAbout inherits Window {
        title: "关于 Flaragon 安装程序";
        preferred-width: 420px; preferred-height: 300px;
        VerticalLayout { padding: 20px; AboutSlint { } }
    }
    export component DeployWindow inherits Window {
        title: "Flaragon 安装与升级";
        preferred-width: 700px; preferred-height: 480px; min-width: 620px; min-height: 440px;
        background: #f6f8fc; default-font-family: "Microsoft YaHei UI"; default-font-size: 13px;
        in property <string> heading;
        in property <string> version;
        in property <string> operation;
        in-out property <string> destination;
        in property <string> status: "请选择独立的使用目录。";
        in property <bool> busy: false;
        in property <bool> failed: false;
        in-out property <bool> reviewed: false;
        in-out property <bool> complete: false;
        callback action(string);
        VerticalLayout { padding: 26px; spacing: 16px;
            Text { text: root.heading; font-size: 26px; font-weight: 700; color: #202c46; }
            Text { text: "Flaragon " + root.version + " · Windows x64"; color: #66738b; }
            Text { text: "仅软件本体。组件由您自行解压添加；升级保留配置、项目和数据库。"; wrap: word-wrap; color: #66738b; }
            Text { text: "软件位置"; }
            HorizontalLayout { spacing: 8px;
                LineEdit { text <=> root.destination; enabled: !root.busy; placeholder-text: "输入完整目录，例如 D:\\Apps\\Flaragon";
                    edited => { root.reviewed = false; root.complete = false; }
                }
                Button { text: "选择目录"; enabled: !root.busy; clicked => { root.action("browse"); } }
            }
            Rectangle { background: #ffffff; border-radius: 8px; border-width: 1px; border-color: #e5eaf3;
                VerticalLayout { padding: 16px;
                    details := ScrollView {
                        horizontal-scrollbar-policy: always-off;
                        content-width: self.visible-width;
                        content-height: message.preferred-height;
                        message := Text {
                            x: 0px; y: 0px;
                            width: details.visible-width - 16px;
                            height: self.preferred-height;
                            text: root.status; wrap: char-wrap;
                            horizontal-alignment: left; vertical-alignment: top;
                            color: root.failed ? #b43c48 : #48576f;
                        }
                    }
                }
            }
            HorizontalLayout { spacing: 8px;
                Button { text: "检查目录"; enabled: !root.busy; clicked => { root.action("inspect"); } }
                Button { text: root.operation; enabled: !root.busy && root.reviewed && !root.complete; clicked => { root.action("apply"); } }
                Button { text: "打开 Flaragon"; enabled: !root.busy && root.complete; clicked => { root.action("open"); } }
                Button { text: "关于"; enabled: !root.busy; clicked => { root.action("about"); } }
            }
            Text { text: "升级前请在 Flaragon 中“停止并退出”。未退出将终止更新。"; font-size: 11px; color: #66738b; wrap: word-wrap; }
        }
    }
}

fn run() -> Result<()> {
    let package = Arc::new(Package::open(&std::env::current_exe()?)?);
    if let Some(action) = std::env::args().nth(1) {
        let root = PathBuf::from(std::env::args_os().nth(2).context("缺少目标目录")?);
        let result = match action.as_str() {
            "--inspect" => deployment::inspect_target(&package, &root),
            "--apply" => deployment::deploy(&package, &root),
            _ => flaragon::bail!("不支持的部署参数"),
        };
        println!(
            "{}",
            match &result {
                Ok(s) => serde_json::json!({"ok":true,"message":s}),
                Err(e) => serde_json::json!({"ok":false,"error":format!("{e:#}")}),
            }
        );
        if result.is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let ui = DeployWindow::new()?;
    let about = std::rc::Rc::new(DeployAbout::new()?);
    ui.set_heading(
        if package.mode == Mode::Unified {
            "安装与升级 Flaragon"
        } else if package.mode == Mode::Install {
            "安装 Flaragon"
        } else if package.mode == Mode::Repair {
            "修复 Flaragon"
        } else {
            "升级 Flaragon"
        }
        .into(),
    );
    ui.set_operation(
        if package.mode == Mode::Unified {
            "开始安装或升级"
        } else if package.mode == Mode::Install {
            "开始安装"
        } else if package.mode == Mode::Repair {
            "开始修复"
        } else {
            "开始升级"
        }
        .into(),
    );
    ui.set_version(package.manifest.version.clone().into());
    ui.set_status("请选择软件位置，然后点击“检查目录”。".into());
    let reviewed = Arc::new(Mutex::new(None::<deployment::Review>));
    let reviewed_state = reviewed.clone();
    let reviewed_package = package.clone();
    let (sender, receiver) = mpsc::sync_channel::<(String, std::result::Result<String, String>)>(2);
    let weak = ui.as_weak();
    ui.on_action(move |action| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_busy() {
            return;
        }
        if action == "about" {
            let _ = about.show();
            return;
        }
        let root = PathBuf::from(ui.get_destination().as_str());
        let package = package.clone();
        let sender = sender.clone();
        let action = action.to_string();
        let reviewed = reviewed.clone();
        ui.set_busy(true);
        ui.set_failed(false);
        ui.set_status("正在处理，请稍候…".into());
        std::thread::spawn(move || {
            let result = match action.as_str() {
                "inspect" => deployment::review_target(&package, &root).and_then(|review| {
                    let text = review.description(&package);
                    *reviewed
                        .lock()
                        .map_err(|_| flaragon::anyhow!("目录检查状态不可用"))? = Some(review);
                    Ok(text)
                }),
                "apply" => reviewed
                    .lock()
                    .map_err(|_| flaragon::anyhow!("目录检查状态不可用"))
                    .and_then(|review| {
                        deployment::deploy_reviewed(
                            &package,
                            &root,
                            review.as_ref().context("请先检查目录")?,
                        )
                    }),
                "open" => deployment::open_installed(&root).map(|_| "已打开 Flaragon。".into()),
                "browse" => flaragon::platform::deployment::choose_directory(&root).map(|p| {
                    p.map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                }),
                _ => Err(flaragon::anyhow!("未知操作")),
            }
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send((action, result));
        });
    });
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(80),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Ok((action, result)) = receiver.try_recv() {
                ui.set_busy(false);
                match result {
                    Ok(message) => {
                        if action == "browse" {
                            if !message.is_empty() {
                                ui.set_destination(message.into());
                            }
                            ui.set_reviewed(false);
                            ui.set_complete(false);
                            ui.set_status("请点击“检查目录”核对目标位置。".into());
                        } else {
                            if action == "inspect" {
                                if let Ok(review) = reviewed_state.lock()
                                    && let Some(review) = review.as_ref()
                                {
                                    ui.set_operation(review.button_label(&reviewed_package).into());
                                }
                                ui.set_reviewed(true);
                            }
                            if action == "apply" {
                                ui.set_complete(true);
                            }
                            ui.set_status(message.into());
                        }
                    }
                    Err(error) => {
                        ui.set_reviewed(false);
                        ui.set_complete(false);
                        ui.set_failed(true);
                        ui.set_status(error.into());
                    }
                }
            }
        },
    );
    let weak = ui.as_weak();
    ui.window().on_close_requested(move || {
        if weak.upgrade().is_some_and(|ui| ui.get_busy()) {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });
    ui.run()?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        if std::env::args_os().nth(1).is_some() {
            println!(
                "{}",
                serde_json::json!({"ok":false,"error":format!("{error:#}")})
            );
        } else {
            flaragon::ui::show_error(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}
