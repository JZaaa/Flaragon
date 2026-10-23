use crate::{Context, Result, config::*, ensure, platform};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

fn owner(layout: &Layout) -> Result<String> {
    let path = layout.path("config/environment.id")?;
    let id = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        let id = platform::random_token()?;
        atomic_write(&path, id.as_bytes())?;
        id
    };
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "环境标识无效"
    );
    Ok(id)
}
pub fn domains(config: &Config) -> Result<Vec<String>> {
    let mut domains = vec![];
    for p in config
        .projects
        .iter()
        .filter(|p| p.enabled && p.virtual_host)
    {
        valid_domain(&p.domain)?;
        let domain = p.domain.to_ascii_lowercase();
        ensure!(
            domain.ends_with(".test") || domain.ends_with(".localhost"),
            "自动 hosts 映射仅接受 .test / .localhost 开发域名：{domain}"
        );
        domains.push(domain);
    }
    domains.sort();
    domains.dedup();
    Ok(domains)
}
pub fn preview(layout: &Layout, config: &Config) -> Result<String> {
    let mapping = merge("", &domains(config)?, &owner(layout)?)?;
    Ok(format!(
        "以下内容仅为当前启用项目的域名映射预览，不会自动修改系统 hosts 文件。\r\n点击“应用域名映射”后，需要管理员授权才能写入。\r\n只更新本环境添加的记录；其他已有记录保持原样，发生冲突时不修改。\r\n.localhost 域名通常由浏览器直接指向本机，无需写入 hosts。\r\n\r\n{mapping}"
    ))
}
pub fn merge(existing: &str, domains: &[String], owner: &str) -> Result<String> {
    ensure!(
        owner.len() == 32 && owner.bytes().all(|b| b.is_ascii_hexdigit()),
        "环境标识无效"
    );
    let begin = format!("# BEGIN FLARAGON {owner}");
    let end = format!("# END FLARAGON {owner}");
    let newline = if existing.contains("\r\n") || existing.is_empty() {
        "\r\n"
    } else {
        "\n"
    };
    let mut kept = String::new();
    let mut inside = false;
    let mut seen = false;
    for line in existing.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        if text == begin {
            ensure!(!inside && !seen, "hosts 标记块重复或嵌套");
            inside = true;
            seen = true;
            continue;
        }
        if text == end {
            ensure!(inside, "hosts 结束标记缺少起始标记");
            inside = false;
            continue;
        }
        if !inside {
            kept.push_str(line);
        }
    }
    ensure!(!inside, "hosts 标记块不完整，拒绝覆盖");
    for domain in domains {
        valid_domain(domain)?;
    }
    let mut present = std::collections::BTreeSet::new();
    for line in kept.lines() {
        let content = line.split('#').next().unwrap_or("");
        let mut fields = content.split_whitespace();
        if let Some(ip) = fields.next() {
            for name in fields {
                if domains.iter().any(|d| d.eq_ignore_ascii_case(name)) {
                    ensure!(
                        ip == "127.0.0.1" || ip == "::1",
                        "已有 hosts 映射冲突：{name} -> {ip}"
                    );
                    present.insert(name.to_ascii_lowercase());
                }
            }
        }
    }
    if !domains.is_empty() {
        if !kept.is_empty() && !kept.ends_with('\n') {
            kept.push_str(newline);
        }
        kept.push_str(&begin);
        kept.push_str(newline);
        for domain in domains {
            if !present.contains(&domain.to_ascii_lowercase()) {
                kept.push_str(&format!("127.0.0.1 {domain}{newline}"));
            }
        }
        kept.push_str(&end);
        kept.push_str(newline);
    }
    Ok(kept)
}

/// Elevated entry point has one fixed system target; it does not accept arbitrary output paths.
pub fn apply_system(layout: &Layout) -> Result<()> {
    let config = layout.load()?;
    let names = domains(&config)?;
    let owner = owner(layout)?;
    let path = platform::system_hosts_path()?;
    reject_links(&path)?;
    apply_file(&path, &layout.path("config/hosts.backup")?, &names, &owner)
}
pub fn apply_file(path: &Path, backup: &Path, names: &[String], owner: &str) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    reject_links(path)?;
    reject_links(backup)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)
        .context("无法独占打开 hosts，请检查权限或其他程序占用")?;
    ensure!(
        file.metadata()?.len() <= 1024 * 1024,
        "hosts 文件超过 1 MiB，拒绝自动处理"
    );
    let mut original = vec![];
    file.read_to_end(&mut original)?;
    let text = std::str::from_utf8(&original)
        .context("hosts 不是 UTF-8 / ASCII，请手动管理以保留原编码")?;
    let candidate = merge(text, names, owner)?;
    if candidate.as_bytes() == original {
        return Ok(());
    }
    atomic_write(backup, &original)?;
    let write = (|| -> Result<()> {
        file.seek(SeekFrom::Start(0))?;
        file.write_all(candidate.as_bytes())?;
        file.set_len(candidate.len() as u64)?;
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let mut verify = String::new();
        file.read_to_string(&mut verify)?;
        ensure!(verify == candidate, "hosts 写入验证失败");
        Ok(())
    })();
    if let Err(error) = write {
        let restored = (|| -> Result<()> {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&original)?;
            file.set_len(original.len() as u64)?;
            file.sync_all()?;
            Ok(())
        })();
        crate::bail!(
            "hosts 应用失败：{error}；恢复：{}",
            restored
                .map(|_| "成功".to_string())
                .unwrap_or_else(|e| e.to_string())
        );
    }
    Ok(())
}
