#![windows_subsystem = "windows"]

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--deployment-check")) {
        let result = (|| -> flaragon::Result<()> {
            use flaragon::Context;
            let expected = std::env::args().nth(2).context("缺少自检版本")?;
            let root = std::env::args_os().nth(3).context("缺少自检目录")?;
            flaragon::deployment::self_check(std::path::Path::new(&root), &expected)
        })();
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--apply-hosts")) {
        let result = (|| -> flaragon::Result<()> {
            use flaragon::Context;
            let root = std::env::args_os().nth(2).context("缺少环境目录")?;
            let nonce = std::env::args().nth(3).context("缺少请求标识")?;
            flaragon::ensure!(
                nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()),
                "请求标识无效"
            );
            let layout = flaragon::config::Layout::new(root)?;
            layout.path(std::env::current_exe()?)?;
            let result = flaragon::hosts::apply_system(&layout);
            let response = serde_json::json!({"ok":result.is_ok(),"error":result.err().map(|e|format!("{e:#}"))});
            flaragon::config::atomic_write(
                &layout.path(format!("runtime/state/hosts-{nonce}.json"))?,
                &serde_json::to_vec(&response)?,
            )?;
            Ok(())
        })();
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    if let Err(error) = flaragon::ui::run() {
        flaragon::ui::show_error(&format!("{error:#}"));
    }
}
