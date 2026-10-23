param(
    [Parameter(Mandatory=$true)][string]$RepairId,
    [Parameter(Mandatory=$true)][string]$Description,
    [string]$BaseRepairId,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
if (-not $SkipBuild) { & "$PSScriptRoot\build.ps1" }
$taskRepairArgs = @('--repair-id', $RepairId, '--description', $Description)
if ($BaseRepairId) { $taskRepairArgs += @('--base-repair-id', $BaseRepairId) }
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\package-repair.py" @taskRepairArgs
if ($LASTEXITCODE -ne 0) { throw '修复包生成失败' }
