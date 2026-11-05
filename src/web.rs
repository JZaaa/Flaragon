//! Apache HTTP entries and a static, read-only project directory.
use crate::{Result, config::*, ensure};
use std::{fs, path::Path};

pub fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn navigation(config: &Config) -> String {
    navigation_with_packages(config, &[])
}
pub fn navigation_with_packages(config: &Config, packages: &[Package]) -> String {
    let mut cards = String::new();
    let local_only = config
        .http_address
        .parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_loopback());
    for p in config
        .projects
        .iter()
        .filter(|p| p.enabled && p.listed && (p.lan_access || local_only))
    {
        let id = p.php.as_ref().or(config.default_php.as_ref());
        let php = id
            .map(|id| {
                packages
                    .iter()
                    .find(|package| package.kind == Kind::Php && &package.id == id)
                    .map(|package| format!("PHP {} · {}", package.version, package.label()))
                    .unwrap_or_else(|| format!("PHP · {id}（版本未识别）"))
            })
            .unwrap_or_else(|| "PHP 未选择".into());
        cards.push_str(&format!("<a class=card href=\"/{}/\" data-group=\"{}\"><strong>{}</strong><span>{}</span><span class=php>{}</span><code>/{}/</code></a>", p.id, html(&p.group), html(p.label()), html(&p.group), html(&php), p.id));
    }
    format!(
        r##"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Flaragon · 项目</title>
<style>body{{margin:0;background:#f5f7fb;color:#273449;font:16px system-ui,"Microsoft YaHei",sans-serif}}main{{max-width:1080px;margin:64px auto;padding:0 24px}}header{{display:flex;align-items:baseline;justify-content:space-between;gap:20px}}h1{{font-size:30px}}header span{{color:#667085}}.filters{{display:flex;gap:12px;margin:24px 0}}input,select{{font:inherit;padding:12px;border:1px solid #d8deea;border-radius:8px;background:white}}input{{flex:1;min-width:0}}#projects{{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:16px}}.card{{display:flex;flex-direction:column;gap:12px;padding:24px;background:white;border:1px solid #e0e5ef;border-radius:12px;color:inherit;text-decoration:none;overflow-wrap:anywhere}}.card:hover{{border-color:#6158dc;box-shadow:0 3px 12px #ddd}}.card span{{color:#68768a;font-size:14px;min-height:20px}}code{{color:#5146c9}}[hidden]{{display:none!important}}#empty{{color:#68768a}}</style>
<main><header><h1>项目</h1><span>Flaragon</span></header><div class=filters><input id=search type=search placeholder="搜索名称或访问路径" aria-label="搜索项目"><select id=group aria-label="项目分组"><option value="">全部分组</option></select></div><section id=projects>{cards}</section><p id=empty hidden>没有匹配的项目</p></main>
<script>const cards=[...document.querySelectorAll('.card')],q=document.querySelector('#search'),g=document.querySelector('#group');[...new Set(cards.map(c=>c.dataset.group).filter(Boolean))].sort().forEach(v=>g.add(new Option(v,v)));function filter(){{let n=0;for(const c of cards){{c.hidden=!(c.textContent.toLowerCase().includes(q.value.toLowerCase())&&(!g.value||g.value===c.dataset.group));if(!c.hidden)n++;}}document.querySelector('#empty').hidden=n>0;}}q.addEventListener('input',filter);g.addEventListener('change',filter);filter();</script></html>"##
    )
}

fn directory(path: &str, access: &str) -> String {
    format!(
        "<Directory \"{path}\">\nOptions +Indexes +FollowSymLinks\nAllowOverride All\nRequire {access}\n</Directory>\n"
    )
}
fn handler(php: &str) -> String {
    format!("<FilesMatch (?i)[.]php$>\nSetHandler \"proxy:balancer://{php}/\"\n</FilesMatch>\n")
}
fn diagnostic(url: &str, file: &str, php: &str, root: &str) -> String {
    format!(
        "Alias \"{url}\" \"{file}\"\n<Location \"{url}\">\nRequire local\nSetHandler \"proxy:balancer://{php}/\"\nProxyFCGISetEnvIf \"true\" DOCUMENT_ROOT \"{root}\"\n</Location>\n"
    )
}

pub fn routes(layout: &Layout, config: &Config, generated: &Path, token: &str) -> Result<String> {
    let portal = layout.path(generated.join("portal"))?;
    fs::create_dir_all(&portal)?;
    atomic_write(
        &layout.path(portal.join("index.html"))?,
        navigation(config).as_bytes(),
    )?;
    let portal = apache_path(&portal)?;
    let health = apache_path(&layout.path(generated.join("health.php"))?)?;
    let mut text = String::new();
    for php in &config.php {
        text.push_str(&format!("<Proxy \"balancer://{}\">\n", php.package));
        for offset in 0..php.workers {
            text.push_str(&format!(
                "BalancerMember \"fcgi://127.0.0.1:{}\"\n",
                php.port + offset
            ));
        }
        text.push_str("</Proxy>\n");
    }
    // The first virtual host is the IP entry; unknown Host headers never select a project.
    text.push_str(&format!(
        "<VirtualHost *:{}>\nServerName localhost\nDocumentRoot \"{portal}\"\n",
        config.http_port
    ));
    text.push_str(&format!("<Directory \"{portal}\">\nOptions -Indexes\nAllowOverride None\nRequire all granted\nDirectoryIndex index.html\n</Directory>\n"));
    for p in config.projects.iter().filter(|p| p.enabled) {
        let doc = layout.project_root(p)?;
        ensure!(doc.is_dir(), "网站入口目录不存在：{}", doc.display());
        let doc = apache_path(&doc)?;
        let php = p
            .php
            .as_ref()
            .or(config.default_php.as_ref())
            .ok_or_else(|| crate::anyhow!("项目未绑定 PHP"))?;
        let access = if p.lan_access { "all granted" } else { "local" };
        text.push_str(&diagnostic(
            &format!("/_flaragon_{token}_{}.php", p.id),
            &health,
            php,
            &doc,
        ));
        text.push_str(&format!(
            "RedirectMatch 308 ^/{}$ /{}/\nAlias \"/{}/\" \"{doc}/\"\n",
            p.id, p.id, p.id
        ));
        text.push_str(&directory(&doc, "all granted"));
        text.push_str(&format!("<Location \"/{}/\">\nRequire {access}\nProxyFCGISetEnvIf \"true\" DOCUMENT_ROOT \"{doc}\"\n</Location>\n<LocationMatch \"^/{}/.*(?i:[.]php)(?:/.*)?$\">\nSetHandler \"proxy:balancer://{php}/\"\n</LocationMatch>\n", p.id, p.id));
    }
    text.push_str("</VirtualHost>\n");
    for p in config
        .projects
        .iter()
        .filter(|p| p.enabled && p.virtual_host)
    {
        let doc = apache_path(&layout.project_root(p)?)?;
        let php = p
            .php
            .as_ref()
            .or(config.default_php.as_ref())
            .ok_or_else(|| crate::anyhow!("项目未绑定 PHP"))?;
        text.push_str(&format!(
            "<VirtualHost *:{}>\nServerName {}\nDocumentRoot \"{doc}\"\n",
            config.http_port, p.domain
        ));
        text.push_str(&directory(
            &doc,
            if p.lan_access { "all granted" } else { "local" },
        ));
        text.push_str(&handler(php));
        text.push_str(&diagnostic(
            &format!("/_flaragon_{token}.php"),
            &health,
            php,
            &doc,
        ));
        text.push_str("</VirtualHost>\n");
    }
    for php in config.php.iter().filter(|p| p.directory_entry.enabled) {
        let entry = &php.directory_entry;
        let doc = layout.site_path(&entry.directory)?;
        ensure!(doc.is_dir(), "目录入口根目录不存在：{}", doc.display());
        let doc = apache_path(&doc)?;
        text.push_str(&format!(
            "<VirtualHost *:{}>\nServerName localhost\nDocumentRoot \"{doc}\"\n",
            entry.port
        ));
        text.push_str(&directory(&doc, "all granted"));
        text.push_str(&handler(&php.package));
        text.push_str(&diagnostic(
            &format!("/_flaragon_{token}.php"),
            &health,
            &php.package,
            &doc,
        ));
        text.push_str("</VirtualHost>\n");
    }
    Ok(text)
}
