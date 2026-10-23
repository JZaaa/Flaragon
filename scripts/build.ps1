$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
foreach ($taskDll in @('Scintilla.dll', 'Lexilla.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $taskRoot ('lib\' + $taskDll)))) { throw '缺少编辑器依赖，请运行 scripts/prepare-editor.ps1' }
}
cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) { throw 'rustfmt 检查失败' }
cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'Clippy 检查失败' }
cargo test --workspace --locked --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw '测试失败' }
cargo build --release --locked --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Release 构建失败' }
Write-Output '检查通过，构建产物位于 target/x86_64-pc-windows-msvc/release；未覆盖任何使用目录。运行 scripts/package.ps1 生成安装和升级 EXE。'
