//! Adapt native package configuration while preserving comments and user directives.
use crate::{Context, Result, config::*, ensure};
use std::{collections::BTreeSet, fs, path::Path};

pub struct Template {
    pub source: String,
    pub text: String,
}

pub fn key(line: &str) -> String {
    line.trim()
        .split(['=', ' ', '\t'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace('_', "-")
}

pub fn load(layout: &Layout, directory: &Path, kind: Kind, head: &str) -> Result<Template> {
    let names: &[&str] = match kind {
        Kind::Php => &["php.ini", "php.ini-development", "php.ini-production"],
        Kind::Apache => &["conf/httpd.conf"],
        Kind::Redis => &["redis.conf", "redis.windows.conf"],
        Kind::Mysql => &["my.ini", "my.cnf", "my-default.ini", "my-default.cnf"],
    };
    for name in names {
        let file = layout.path(directory.join(name))?;
        if !file.is_file()
            || (matches!(*name, "php.ini" | "redis.conf")
                && layout.path(file.with_extension("managed.json"))?.exists())
        {
            continue;
        }
        return from_file(layout, &file, directory, kind, head);
    }
    let comment = if matches!(kind, Kind::Php | Kind::Mysql) {
        ';'
    } else {
        '#'
    };
    Ok(Template {
        source: "no-package-template".into(),
        text: format!(
            "{comment} 此组件包未附带配置模板；以下为实例配置，可直接添加原生参数。\n{comment} 这不是发行包的完整参数清单；未列出的参数使用该版本内置默认值。\n"
        ),
    })
}

pub fn from_file(
    layout: &Layout,
    file: &Path,
    directory: &Path,
    kind: Kind,
    head: &str,
) -> Result<Template> {
    let file = layout.path(file)?;
    ensure!(
        fs::metadata(&file)?.len() <= crate::service_config::MAX_CONFIG_BYTES as u64,
        "组件配置模板超过 1 MiB"
    );
    let original = fs::read_to_string(&file).context("组件配置模板必须为 UTF-8")?;
    let source = file
        .strip_prefix(&layout.root)?
        .to_string_lossy()
        .into_owned();
    let comment = if matches!(kind, Kind::Php | Kind::Mysql) {
        ';'
    } else {
        '#'
    };
    let mut text = format!(
        "{comment} 模板来源：{source}\n{comment} 原始注释与参数保留；实例归属项见文件顶部管理段。\n"
    );
    let managed: BTreeSet<_> = head.lines().map(key).collect();
    for raw in original.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            text.push_str(raw);
            text.push('\n');
            continue;
        }
        let k = key(line);
        let controlled = managed.contains(&k)
            && !line.starts_with(['[', '<'])
            && k != "loadmodule"
            && (kind != Kind::Apache
                || matches!(
                    k.as_str(),
                    "serverroot"
                        | "listen"
                        | "pidfile"
                        | "errorlog"
                        | "defaultruntimedir"
                        | "typesconfig"
                        | "proxyrequests"
                        | "proxyfcgibackendtype"
                ));
        let disabled = controlled
            || match kind {
                Kind::Apache => {
                    matches!(k.as_str(), "define" | "include" | "includeoptional")
                        || (k == "loadmodule"
                            && head.lines().any(|l| {
                                key(l) == k
                                    && l.split_whitespace().nth(1) == line.split_whitespace().nth(1)
                            }))
                }
                Kind::Redis => matches!(
                    k.as_str(),
                    "daemonize" | "pidfile" | "appendfilename" | "appenddirname" | "supervised"
                ),
                _ => false,
            };
        if disabled {
            text.push_str(&format!(
                "{comment} Flaragon：由实例管理或需单独配置文件引用\n{comment} {raw}\n"
            ));
            continue;
        }
        let mut adapted = raw.to_owned();
        if kind == Kind::Php && k == "soap.wsdl-cache-dir" {
            adapted = format!(
                "soap.wsdl_cache_dir=\"{}\"",
                apache_path(&layout.path("runtime/temp")?)?
            );
        }
        if kind == Kind::Apache {
            adapted = adapted.replace("${SRVROOT}", &apache_path(directory)?);
            if k == "loadmodule" {
                let parts: Vec<_> = line.split_whitespace().collect();
                ensure!(parts.len() == 3, "Apache 模板 LoadModule 格式无效");
                let module = layout.path(directory.join(parts[2].trim_matches('"')))?;
                adapted = format!("LoadModule {} \"{}\"", parts[1], apache_path(&module)?);
            }
            if k == "customlog" {
                let parts: Vec<_> = line.split_whitespace().collect();
                ensure!(
                    parts.len() == 3 && parts[1] == "\"logs/access_log\"",
                    "Apache 模板 CustomLog 需要显式设置环境内路径"
                );
                adapted = format!(
                    "CustomLog \"{}\" {}",
                    apache_path(&layout.path("logs/apache-access.log")?)?,
                    parts[2]
                );
            }
        }
        text.push_str(&adapted);
        text.push('\n');
    }
    Ok(Template { source, text })
}

/// Existing directives retain their original order and scope and override template defaults.
pub fn merge(kind: Kind, template: &str, existing: &str) -> String {
    let comment = if matches!(kind, Kind::Php | Kind::Mysql) {
        ';'
    } else {
        '#'
    };
    let mut group = if kind == Kind::Mysql {
        "[mysqld]".to_owned()
    } else {
        String::new()
    };
    let mut keys = BTreeSet::new();
    let mut depth = 0usize;
    for line in existing.lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if kind == Kind::Mysql && line.starts_with('[') {
            group = line.to_ascii_lowercase();
            continue;
        }
        if kind == Kind::Apache && line.starts_with('<') {
            if line.starts_with("</") {
                depth = depth.saturating_sub(1);
            } else {
                depth += 1;
            }
            continue;
        }
        if depth == 0 {
            keys.insert((group.clone(), key(line)));
        }
    }
    group = if kind == Kind::Mysql {
        "[mysqld]".into()
    } else {
        String::new()
    };
    let mut text = String::new();
    for raw in template.lines() {
        let line = raw.trim();
        if kind == Kind::Mysql && line.starts_with('[') {
            group = line.to_ascii_lowercase();
        }
        if !line.is_empty()
            && !line.starts_with([';', '#', '[', '<'])
            && keys.contains(&(group.clone(), key(line)))
        {
            text.push_str(&format!("{comment} 实例自定义值见下方\n{comment} {raw}\n"));
        } else {
            text.push_str(raw);
            text.push('\n');
        }
    }
    text.push_str(&format!("\n{comment} 实例自定义配置\n"));
    match kind {
        Kind::Php => text.push_str("[PHP]\n"),
        Kind::Mysql => text.push_str("[mysqld]\n"),
        _ => {}
    }
    text.push_str(existing);
    text
}
