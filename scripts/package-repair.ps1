param(
    [Parameter(Mandatory=$true)][string]$RepairId,
    [Parameter(Mandatory=$true)][string]$Description,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
if (-not $SkipBuild) { & "$PSScriptRoot\build.ps1" }
& "$taskRoot\.tools\python\python.exe" "$PSScriptRoot\package-repair.py" --repair-id $RepairId --description $Description
if ($LASTEXITCODE -ne 0) { throw '修复包生成失败' }
