param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
if (-not $SkipBuild) {
    & "$PSScriptRoot\build.ps1"
}
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\collect-licenses.py"
if ($LASTEXITCODE -ne 0) { throw '许可文件收集失败' }
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\package.py"
if ($LASTEXITCODE -ne 0) { throw '发布包生成失败' }
