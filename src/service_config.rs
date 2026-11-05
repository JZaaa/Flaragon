//! Persistent native service configuration and bounded, managed instance sections.
use crate::{Context, Result, config::*, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Managed {
    head: String,
    tail: String,
    #[serde(default)]
    template: Option<String>,
}

pub fn needs_template(layout: &Layout, file: &Path) -> Result<bool> {
    let file = layout.path(file)?;
    if !file.exists() || !layout.path(metadata(&file))?.exists() {
        return Ok(true);
    }
    let managed: Managed = serde_json::from_slice(&fs::read(layout.path(metadata(&file))?)?)?;
    Ok(managed.template.is_none()
        || managed.head.lines().any(|line| {
            matches!(
                crate::config_template::key(line).as_str(),
                "bind" | "bind-address" | "protected-mode"
            )
        }))
}

/// Resolve the native file within the selected, already discovered component package.
pub fn package_path(layout: &Layout, package: &Package, id: &str) -> Result<PathBuf> {
    let name = match package.kind {
        Kind::Php => "php.ini".to_owned(),
        Kind::Redis => "redis.conf".to_owned(),
        Kind::Mysql => {
            let instance = id.strip_prefix("mysql:").context("MySQL 实例标识无效")?;
            valid_id(instance)?;
            format!("instances/{instance}/my.ini")
        }
        Kind::Apache => return layout.path("config/services/apache/httpd.conf"),
    };
    let directory = layout.path(&package.directory)?;
    ensure!(
        directory.starts_with(layout.path(format!("bin/{}", package.kind.folder()))?),
        "配置不属于所选组件包"
    );
    layout.path(directory.join(name))
}

fn metadata(path: &Path) -> PathBuf {
    path.with_extension("managed.json")
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn section(kind: Kind, name: &str, body: &str) -> String {
    let comment = if matches!(kind, Kind::Php | Kind::Mysql) {
        ';'
    } else {
        '#'
    };
    format!(
        "{comment} BEGIN FLARAGON {name}\n{comment} 此段由环境设置和项目页维护；在段外编辑原生配置参数。\n{body}{comment} END FLARAGON {name}\n"
    )
}

fn user_text(text: &str, managed: &Managed) -> Result<String> {
    let text = normalize(text);
    let body = text.strip_prefix(&managed.head)
        .and_then(|text| text.strip_suffix(&managed.tail))
        .context("实例管理段被修改或移除；端口、数据目录和项目路由请在环境设置或项目页修改，其余原生参数可直接编辑")?;
    ensure!(
        !body.contains("BEGIN FLARAGON") && !body.contains("END FLARAGON"),
        "配置管理标记重复"
    );
    Ok(body.to_owned())
}

pub fn read(layout: &Layout, file: &Path) -> Result<String> {
    let file = layout.path(file)?;
    ensure!(
        fs::metadata(&file)?.len() <= MAX_CONFIG_BYTES as u64,
        "配置文件超过 1 MiB"
    );
    fs::read_to_string(file).context("配置文件必须使用 UTF-8 编码")
}

pub fn validate_edit(layout: &Layout, file: &Path, kind: Kind, text: &str) -> Result<()> {
    ensure!(text.len() <= MAX_CONFIG_BYTES, "配置文件超过 1 MiB");
    let file = layout.path(file)?;
    let managed: Managed = serde_json::from_slice(&fs::read(layout.path(metadata(&file))?)?)?;
    validate_user(layout, kind, &user_text(text, &managed)?)
}

pub(crate) fn validate_relocated(
    layout: &Layout,
    kind: Kind,
    text: &[u8],
    baseline: &[u8],
) -> Result<()> {
    ensure!(text.len() <= MAX_CONFIG_BYTES, "配置文件超过 1 MiB");
    let managed: Managed = serde_json::from_slice(baseline)?;
    validate_user(
        layout,
        kind,
        &user_text(std::str::from_utf8(text)?, &managed)?,
    )
}

pub fn replace_user(layout: &Layout, file: &Path, text: &str) -> Result<String> {
    let file = layout.path(file)?;
    let managed: Managed = serde_json::from_slice(&fs::read(layout.path(metadata(&file))?)?)?;
    Ok(format!(
        "{}{}\n{}",
        managed.head,
        text.trim_end(),
        managed.tail
    ))
}

/// Build a native candidate without modifying the persistent file or its baseline.
#[allow(clippy::too_many_arguments)] // Each native template has independent managed sections and a one-time user seed.
pub fn prepare(
    layout: &Layout,
    file: &Path,
    kind: Kind,
    head: &str,
    tail: &str,
    seed: &str,
    candidate: &Path,
    template: Option<&crate::config_template::Template>,
) -> Result<PathBuf> {
    let file = layout.path(file)?;
    let mut source = None;
    let body = if file.exists() && layout.path(metadata(&file))?.exists() {
        let managed: Managed = serde_json::from_slice(&fs::read(layout.path(metadata(&file))?)?)?;
        source = managed.template.clone();
        let mut body = user_text(&read(layout, &file)?, &managed)?;
        let editable = managed
            .head
            .lines()
            .filter(|line| {
                let key = line.split(['=', ' ', '\t']).next().unwrap_or("");
                matches!(
                    (kind, key),
                    (Kind::Php, "log_errors")
                        | (Kind::Mysql, "character-set-server" | "bind-address")
                        | (Kind::Redis, "bind" | "protected-mode")
                        | (
                            Kind::Apache,
                            "ServerName" | "LogLevel" | "ThreadsPerChild" | "DirectoryIndex"
                        )
                ) && !head.lines().any(|current| current == *line)
            })
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        body.insert_str(0, &editable);
        body
    } else {
        normalize(seed)
    };
    let native_template = if file.exists() && !layout.path(metadata(&file))?.exists() {
        ensure!(
            !read(layout, &file)?.contains("BEGIN FLARAGON"),
            "配置管理基线缺失，请恢复配套基线后重试"
        );
        Some(crate::config_template::from_file(
            layout,
            &file,
            file.parent().context("配置目录缺失")?,
            kind,
            head,
        )?)
    } else {
        None
    };
    let template = native_template.as_ref().or(template);
    let body = if source.is_none() {
        if let Some(template) = template {
            source = Some(template.source.clone());
            crate::config_template::merge(kind, &template.text, &body)
        } else {
            body
        }
    } else {
        body
    };
    let mut body = body;
    if !file.exists()
        && kind == Kind::Mysql
        && !body
            .lines()
            .any(|line| crate::config_template::key(line) == "bind-address")
    {
        body.push_str("\n[mysqld]\nbind-address=0.0.0.0\n");
    }
    if !file.exists()
        && kind == Kind::Redis
        && !body
            .lines()
            .any(|line| crate::config_template::key(line) == "bind")
    {
        body.push_str("\nbind 0.0.0.0\n");
    }
    validate_user(layout, kind, &body)?;
    let managed = Managed {
        template: source,
        head: section(kind, "INSTANCE", head),
        tail: if tail.is_empty() {
            String::new()
        } else {
            section(kind, "PROJECTS", tail)
        },
    };
    let body = if body.ends_with('\n') {
        body
    } else {
        format!("{body}\n")
    };
    ensure!(
        managed.head.len() + body.len() + managed.tail.len() <= MAX_CONFIG_BYTES,
        "合并后的完整配置超过 1 MiB"
    );
    let candidate = layout.path(candidate)?;
    atomic_write(
        &candidate,
        format!("{}{}{}", managed.head, body, managed.tail).as_bytes(),
    )?;
    atomic_write(
        &layout.path(metadata(&candidate))?,
        &serde_json::to_vec_pretty(&managed)?,
    )?;
    Ok(candidate)
}

pub fn commit(layout: &Layout, file: &Path, kind: Kind, candidate: &Path) -> Result<PathBuf> {
    let file = layout.path(file)?;
    fs::create_dir_all(layout.path(file.parent().context("配置目录缺失")?)?)?;
    let candidate_bytes = fs::read(layout.path(candidate)?)?;
    let candidate_metadata = fs::read(layout.path(metadata(candidate))?)?;
    let old = if file.exists() {
        Some(fs::read(&file)?)
    } else {
        None
    };
    if old.as_ref() != Some(&candidate_bytes) {
        if let Some(bytes) = &old {
            let backup = if kind == Kind::Redis {
                crate::platform::secrets::seal(bytes)?
            } else {
                bytes.clone()
            };
            atomic_write(&layout.path(file.with_extension("backup"))?, &backup)?;
        }
        atomic_write(&file, &candidate_bytes)?;
    }
    if let Err(error) = atomic_write_if_changed(&layout.path(metadata(&file))?, &candidate_metadata)
    {
        if let Some(bytes) = old {
            atomic_write(&file, &bytes).context("配置基线保存失败，原文件恢复失败")?;
        }
        return Err(error.context("配置基线保存失败；请检查配置文件和备份"));
    }
    Ok(file)
}

pub fn snapshot(layout: &Layout, file: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let file = layout.path(file)?;
    [file.clone(), layout.path(metadata(&file))?]
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| Ok((p.clone(), fs::read(p)?)))
        .collect()
}

/// Native scalar directives are accepted; instance ownership and indirect file execution stay explicit.
pub fn validate_user(layout: &Layout, kind: Kind, text: &str) -> Result<()> {
    ensure!(!text.contains('\0'), "配置含空字符");
    if kind == Kind::Redis {
        crate::redis_config::password(text)?;
    }
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        let key = line
            .split(|c: char| c == '=' || c.is_ascii_whitespace() || c == '\u{b}')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('_', "-");
        let key = if kind == Kind::Mysql {
            key.strip_prefix("loose-").unwrap_or(&key).to_owned()
        } else {
            key
        };
        ensure!(
            !key.contains("password") && !key.contains("passwd"),
            "凭据不能保存在配置快照中"
        );
        let denied: &[&str] = match kind {
            Kind::Php => &[
                "extension-dir",
                "sys-temp-dir",
                "upload-tmp-dir",
                "session.save-path",
                "error-log",
                "cgi.force-redirect",
                "cgi.fix-pathinfo",
            ],
            Kind::Mysql => &[
                "basedir",
                "datadir",
                "port",
                "log-error",
                "pid-file",
                "tmpdir",
                "mysqlx",
                "mysqlx-port",
                "mysqlx-bind-address",
                "socket",
                "shared-memory",
                "named-pipe",
                "daemonize",
                "initialize",
                "initialize-insecure",
                "init-file",
                "plugin-load",
                "plugin-load-add",
                "early-plugin-load",
                "plugin-dir",
                "secure-file-priv",
                "local-infile",
                "log-syslog",
                "log-error-services",
                "innodb-data-home-dir",
                "innodb-data-file-path",
                "innodb-log-group-home-dir",
                "innodb-undo-directory",
                "innodb-directories",
                "innodb-temp-data-file-path",
                "innodb-temp-tablespaces-dir",
            ],
            Kind::Redis => &[
                "port",
                "tls-port",
                "dir",
                "dbfilename",
                "appendfilename",
                "appenddirname",
                "logfile",
                "pidfile",
                "daemonize",
                "supervised",
                "replicaof",
                "slaveof",
                "cluster-enabled",
                "cluster-config-file",
                "loadmodule",
                "masterauth",
                "aclfile",
                "user",
                "rename-command",
            ],
            Kind::Apache => &[
                "serverroot",
                "listen",
                "pidfile",
                "defaultruntimedir",
                "errorlog",
                "define",
                "undefine",
                "loadfile",
                "<virtualhost",
                "<proxy",
                "balancermember",
                "proxypass",
                "proxypassmatch",
                "proxyremote",
                "rewritemap",
            ],
        };
        ensure!(
            !denied.contains(&key.as_str())
                && !key.starts_with("!include")
                && !matches!(key.as_str(), "include" | "includeoptional"),
            "第 {} 行的 {} 涉及实例归属、外部引用或控制通道，不能在用户配置区覆盖",
            index + 1,
            key
        );
        if kind == Kind::Redis && key == "requirepass" {
            continue;
        }
        ensure!(
            !line.contains("${")
                && !line.ends_with('\\')
                && (kind == Kind::Php || !line.contains('|')),
            "第 {} 行包含无法校验的变量、续行或命令管道",
            index + 1
        );
        if line.starts_with('[') {
            ensure!(
                matches!(kind, Kind::Php | Kind::Mysql) && line.ends_with(']'),
                "配置节格式错误"
            );
            if kind == Kind::Mysql {
                ensure!(
                    matches!(line, "[mysqld]" | "[client]" | "[mysql]" | "[mysqldump]"),
                    "不支持该 MySQL 配置组"
                );
            }
            continue;
        }
        if kind == Kind::Php || (kind == Kind::Mysql && line.contains('=')) {
            let (name, value) = line.split_once('=').context("INI 配置应使用 key=value")?;
            if matches!(name.trim(), "extension" | "zend_extension") {
                let value = value.trim().trim_matches(['\'', '"']);
                ensure!(
                    !value.is_empty()
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'),
                    "扩展必须是当前组件 ext 目录内的文件名"
                );
            }
        }
        // Resolve file references before invoking a parser; relative bases differ between components.
        let mut quoted = false;
        let mut quote = '"';
        let mut token = String::new();
        let mut tokens = vec![];
        for c in line.chars() {
            if quoted {
                if c == quote {
                    quoted = false;
                } else {
                    token.push(c);
                }
            } else if c == '"' || c == '\'' {
                quoted = true;
                quote = c;
            } else if c == ';' && matches!(kind, Kind::Php | Kind::Mysql) {
                break;
            } else if c.is_whitespace() || c == '=' {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            } else {
                token.push(c);
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
        let file_directive = matches!(
            key.as_str(),
            "auto-prepend-file"
                | "auto-append-file"
                | "opcache.preload"
                | "opcache.file-cache"
                | "curl.cainfo"
                | "openssl.cafile"
                | "openssl.capath"
                | "include-path"
                | "open-basedir"
                | "soap.wsdl-cache-dir"
                | "doc-root"
                | "browscap"
                | "general-log-file"
                | "slow-query-log-file"
                | "log-bin"
                | "log-bin-index"
                | "relay-log"
                | "relay-log-index"
                | "ssl-ca"
                | "ssl-capath"
                | "ssl-cert"
                | "ssl-key"
                | "customlog"
                | "transferlog"
                | "documentroot"
                | "typesconfig"
                | "authuserfile"
                | "authgroupfile"
                | "<directory"
                | "alias"
                | "scriptalias"
        );
        for (argument, token) in tokens.iter().enumerate().skip(1) {
            if matches!(
                key.as_str(),
                "date.timezone"
                    | "default-mimetype"
                    | "session.cookie-path"
                    | "logformat"
                    | "addtype"
            ) {
                continue;
            }
            if kind == Kind::Apache && key == "<directory" && token == "/>" {
                continue;
            }
            let alias = matches!(key.as_str(), "alias" | "scriptalias");
            if alias && argument == 1 {
                continue;
            }
            let file_argument = (file_directive && argument == if alias { 2 } else { 1 })
                || (kind == Kind::Apache && key == "loadmodule" && argument == 2);
            if file_argument
                || token.contains('/')
                || token.contains('\\')
                || token.as_bytes().get(1) == Some(&b':')
            {
                ensure!(!token.contains("://"), "配置中的网络目标不能绕过实例隔离");
                for path in token.trim_end_matches('>').split(';') {
                    ensure!(
                        Path::new(path).is_absolute(),
                        "第 {} 行的文件引用必须是环境目录内的绝对路径",
                        index + 1
                    );
                    layout
                        .path(path)
                        .with_context(|| format!("第 {} 行的路径不在环境目录内", index + 1))?;
                }
            }
        }
    }
    Ok(())
}
