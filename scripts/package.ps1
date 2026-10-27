param([switch]$SkipBuild, [string]$BuildId)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\test-release-policy.py"
if ($LASTEXITCODE -ne 0) { throw '升级版本规则检查失败' }
if (-not $SkipBuild) {
    & "$PSScriptRoot\build.ps1"
}
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\collect-licenses.py"
if ($LASTEXITCODE -ne 0) { throw '许可文件收集失败' }
$taskPackageArgs = @()
if ($BuildId) { $taskPackageArgs += @('--build-id', $BuildId) }
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\package.py" @taskPackageArgs
if ($LASTEXITCODE -ne 0) { throw '发布包生成失败' }
