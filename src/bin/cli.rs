use flaragon::{
    Context, Result,
    config::{Config, Layout},
    engine::Engine,
    platform::AppLock,
};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("用法：flaragon-cli <环境根目录>；从 stdin 逐行接收 JSON 命令")?;
    let layout = Layout::prepare(root)?;
    let _lock = AppLock::acquire(&layout.root)?;
    layout.initialize()?;
    let mut engine = Engine::new(layout)?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let result = (|| -> Result<serde_json::Value> {
            let value: serde_json::Value = serde_json::from_str(&line)?;
            let action = value["action"].as_str().context("缺少 action")?;
            let id = value["id"].as_str().unwrap_or_default();
            match action {
                "scan" => engine.scan()?,
                "snapshot" => (),
                "start_all" => engine.start_all()?,
                "start_common" => engine.start_common()?,
                "stop_all" => engine.stop_all()?,
                "start" => engine.start(id)?,
                "stop" => engine.stop(id)?,
                "restart" => engine.restart(id)?,
                "reload" => engine.reload(id)?,
                "apply" => {
                    engine.apply(serde_json::from_value::<Config>(value["config"].clone())?)?
                }
                "settings" => engine
                    .apply_settings(serde_json::from_value::<Config>(value["config"].clone())?)?,
                "override" => {
                    engine.save_override(id, value["text"].as_str().context("缺少 text")?)?
                }
                "read_config" => {
                    let text = engine.read_service_config(id)?;
                    return Ok(
                        serde_json::json!({"ok": true, "snapshot": engine.snapshot(), "text": text,
                        "path": flaragon::service_config::path(&engine.layout, id)?}),
                    );
                }
                "save_config" => {
                    engine.save_service_config(id, value["text"].as_str().context("缺少 text")?)?
                }
                "verify" => engine.verify_sites()?,
                _ => flaragon::bail!("未知命令：{action}"),
            }
            Ok(serde_json::json!({"ok":true,"snapshot":engine.snapshot()}))
        })();
        let response = match result {
            Ok(value) => value,
            Err(error) => {
                serde_json::json!({"ok":false,"error":format!("{error:#}"),"snapshot":engine.snapshot()})
            }
        };
        println!("{response}");
        io::stdout().flush()?;
    }
    Ok(())
}
