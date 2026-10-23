use crate::{
    Result,
    config::{Kind, Layout, Package},
    platform,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Default)]
pub struct Cache(BTreeMap<PathBuf, (SystemTime, u64, Package)>);

pub fn scan(layout: &Layout) -> Result<Vec<Package>> {
    scan_cached(layout, &mut Cache::default())
}
pub fn scan_cached(layout: &Layout, cache: &mut Cache) -> Result<Vec<Package>> {
    let mut packages = vec![];
    for kind in [Kind::Apache, Kind::Php, Kind::Mysql, Kind::Redis] {
        let folder = layout.path(format!("bin/{}", kind.folder()))?;
        for entry in fs::read_dir(folder)? {
            crate::ensure!(
                packages.len() < 128,
                "组件数量超过 128 个，请缩小 bin 目录内容"
            );
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = layout.path(entry.path())?;
            let id = format!(
                "{}-{}",
                kind.folder(),
                entry
                    .file_name()
                    .to_string_lossy()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() {
                        c.to_string()
                    } else if c.is_ascii() {
                        "-".into()
                    } else {
                        format!("u{:x}", c as u32)
                    })
                    .collect::<String>()
            );
            if crate::config::valid_id(&id).is_err() {
                continue;
            }
            let exe_name = match kind {
                Kind::Apache => "httpd.exe",
                Kind::Php => "php.exe",
                Kind::Mysql => "mysqld.exe",
                Kind::Redis => "redis-server.exe",
            };
            let found = find_executable(layout, &path, exe_name, 0)?;
            let mut package = Package {
                id,
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                directory: path.clone(),
                executable: found.clone().unwrap_or_else(|| path.join(exe_name)),
                version: String::new(),
                architecture: String::new(),
                php_thread_safe: None,
                error: None,
            };
            if let Some(exe) = found {
                let exe = layout.path(exe)?;
                package.directory = if matches!(kind, Kind::Apache | Kind::Mysql) {
                    exe.parent()
                        .and_then(Path::parent)
                        .unwrap_or(&path)
                        .to_path_buf()
                } else {
                    exe.parent().unwrap_or(&path).to_path_buf()
                };
                let mut cmd = platform::command(&exe, &layout.root, &package.directory);
                let metadata = fs::metadata(&exe)?;
                let modified = metadata.modified()?;
                if let Some((cached_time, cached_len, cached)) = cache.0.get(&exe)
                    && *cached_time == modified
                    && *cached_len == metadata.len()
                    && cached.error.is_none()
                {
                    let mut cached = cached.clone();
                    if let Err(error) = essential_files(&cached) {
                        cached.error = Some(error.to_string());
                    }
                    packages.push(cached);
                    continue;
                }
                match pe_architecture(&exe) {
                    Ok(0x8664) => package.architecture = "x64".into(),
                    Ok(machine) => {
                        package.error =
                            Some(format!("只支持 x64 可执行文件，PE Machine={machine:04x}"));
                        packages.push(package);
                        continue;
                    }
                    Err(error) => {
                        package.error = Some(error.to_string());
                        packages.push(package);
                        continue;
                    }
                }
                match kind {
                    Kind::Php => {
                        cmd.args(["-n", "-r", "echo PHP_VERSION,'|',PHP_INT_SIZE,'|',PHP_ZTS;"]);
                    }
                    Kind::Apache => {
                        cmd.arg("-v");
                    }
                    Kind::Mysql => {
                        cmd.args(["--no-defaults", "--version"]);
                    }
                    Kind::Redis => {
                        cmd.arg("--version");
                    }
                }
                match platform::capture(&mut cmd, Duration::from_secs(8)) {
                    Ok(text) => {
                        package.version = version(&text);
                        if package.version.is_empty() {
                            package.error = Some(format!("未识别版本：{}", text.trim()));
                        }
                        if kind == Kind::Php {
                            package.php_thread_safe =
                                text.trim().rsplit('|').next().and_then(|v| match v {
                                    "0" => Some(false),
                                    "1" => Some(true),
                                    _ => None,
                                });
                        }
                        if kind == Kind::Php && !text.contains("|8|") {
                            package.error = Some("只支持 x64 PHP".into());
                        }
                        if kind == Kind::Php && !package.directory.join("php-cgi.exe").exists() {
                            package.error = Some("缺少 php-cgi.exe".into());
                        }
                    }
                    Err(e) => package.error = Some(e.to_string()),
                }
                if package.error.is_none()
                    && let Err(error) = essential_files(&package)
                {
                    package.error = Some(error.to_string());
                }
                cache
                    .0
                    .insert(exe, (modified, metadata.len(), package.clone()));
            } else {
                package.error = Some(format!("找不到 {exe_name}（扫描深度 3）"));
            }
            packages.push(package);
        }
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    cache
        .0
        .retain(|path, _| packages.iter().any(|p| p.executable == *path));
    let mut ids = BTreeMap::new();
    for p in &packages {
        *ids.entry(p.id.clone()).or_insert(0usize) += 1;
    }
    for p in &mut packages {
        if ids[&p.id] > 1 {
            p.error = Some("目录名生成的组件标识重复，请重命名组件文件夹".into());
        }
    }
    Ok(packages)
}
fn find_executable(
    layout: &Layout,
    dir: &Path,
    name: &str,
    depth: u8,
) -> Result<Option<std::path::PathBuf>> {
    let path = layout.path(dir.join(name))?;
    if path.is_file() {
        return Ok(Some(path));
    }
    if depth >= 3 {
        return Ok(None);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let dir = layout.path(entry.path())?;
            if let Some(path) = find_executable(layout, &dir, name, depth + 1)? {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}
pub fn version(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| s.split('.').count() == 3 && s.split('.').all(|p| !p.is_empty()))
        .unwrap_or_default()
        .to_string()
}

fn pe_architecture(path: &Path) -> Result<u16> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header)?;
    crate::ensure!(&header[..2] == b"MZ", "无效的 Windows 可执行文件");
    let offset = u32::from_le_bytes([header[60], header[61], header[62], header[63]]);
    crate::ensure!(offset <= 16 * 1024 * 1024, "PE 头偏移无效");
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut pe = [0u8; 6];
    file.read_exact(&mut pe)?;
    crate::ensure!(&pe[..4] == b"PE\0\0", "无效的 PE 签名");
    Ok(u16::from_le_bytes([pe[4], pe[5]]))
}
fn essential_files(package: &Package) -> Result<()> {
    let paths: Vec<PathBuf> = match package.kind {
        Kind::Php => vec![package.directory.join("php-cgi.exe")],
        Kind::Apache => [
            "proxy",
            "proxy_fcgi",
            "proxy_balancer",
            "lbmethod_byrequests",
            "rewrite",
        ]
        .iter()
        .map(|m| package.directory.join(format!("modules/mod_{m}.so")))
        .collect(),
        _ => vec![],
    };
    for path in paths {
        crate::config::reject_links(&path)?;
        crate::ensure!(path.is_file(), "缺少必要文件：{}", path.display());
    }
    Ok(())
}
