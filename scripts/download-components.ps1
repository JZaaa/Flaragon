$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
New-Item -ItemType Directory -Force -Path (Join-Path $taskRoot 'downloads') | Out-Null
function Get-Package([string]$Url, [string]$Name, [string]$Destination, [string]$Executable, [string]$Accept = 'application/octet-stream') {
    $out = [IO.Path]::GetFullPath((Join-Path $taskRoot $Destination))
    if (-not $out.StartsWith($taskRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw '目标超出项目目录' }
    if (Test-Path -LiteralPath $out) {
        $installed = Get-ChildItem -LiteralPath $out -Filter $Executable -File -Recurse -Depth 3 | Select-Object -First 1
        if ($installed) { Write-Output "已存在：$Name（运行面板可检查兼容性）"; return }
        throw "组件目录已存在但不完整，请检查后重试：$out"
    }
    $archive = Join-Path $taskRoot "downloads\$Name.zip"
    $usable = $false
    if (Test-Path -LiteralPath $archive) {
        try {
            $zip = [IO.Compression.ZipFile]::OpenRead($archive)
            try { $usable = $zip.Entries.Count -gt 0 } finally { $zip.Dispose() }
        } catch { $usable = $false }
    }
    if (-not $usable) {
        & C:\Windows\System32\curl.exe -sSL --fail --retry 2 --retry-delay 2 --connect-timeout 20 --max-time 1800 --speed-time 60 --speed-limit 1024 -H "Accept: $Accept" -o "$archive.part" $Url
        if ($LASTEXITCODE -ne 0) { throw "下载失败：$Name；未完成文件保留在 downloads 中" }
        $zip = [IO.Compression.ZipFile]::OpenRead("$archive.part")
        try { if ($zip.Entries.Count -eq 0) { throw '空压缩包' } } finally { $zip.Dispose() }
        Move-Item -LiteralPath "$archive.part" -Destination $archive -Force
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $out
    Write-Output "已准备：$Name"
}
Get-Package 'https://downloads.php.net/~windows/releases/archives/php-7.4.33-nts-Win32-vc15-x64.zip' 'php-7.4.33' 'bin\php\php-7.4.33' 'php.exe'
Get-Package 'https://downloads.php.net/~windows/releases/php-8.3.35-nts-Win32-vs16-x64.zip' 'php-8.3.35' 'bin\php\php-8.3.35' 'php.exe'
Get-Package 'https://downloads.php.net/~windows/releases/php-8.5.11-nts-Win32-vs17-x64.zip' 'php-8.5.11' 'bin\php\php-8.5.11' 'php.exe'
Get-Package 'https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.69-261002-Win64-VS18.zip' 'apache-2.4.69' 'bin\apache\apache-2.4.69' 'httpd.exe'
Get-Package 'https://cdn.mysql.com/archives/mysql-5.7/mysql-5.7.44-winx64.zip' 'mysql-5.7.44' 'bin\mysql\mysql-5.7.44' 'mysqld.exe'
Get-Package 'https://cdn.mysql.com/archives/mysql-8.0/mysql-8.0.44-winx64.zip' 'mysql-8.0.44' 'bin\mysql\mysql-8.0.44' 'mysqld.exe'
Get-Package 'https://api.github.com/repos/redis-windows/redis-windows/releases/assets/248993745' 'redis-7.4.3' 'bin\redis\redis-7.4.3' 'redis-server.exe'
