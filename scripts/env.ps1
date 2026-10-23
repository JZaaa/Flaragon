$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$env:FLARAGON_DEV_ROOT = $taskRoot
$env:CARGO_HOME = Join-Path $taskRoot '.cache\cargo'
$env:RUSTUP_HOME = Join-Path $taskRoot '.cache\rustup'
$env:CARGO_TARGET_DIR = Join-Path $taskRoot 'target'
$env:TEMP = Join-Path $taskRoot '.cache\tmp'
$env:TMP = $env:TEMP
$env:PORTABLEMSVC_CACHE = Join-Path $taskRoot '.cache\msvc'
$env:PORTABLEMSVC_DATA = Join-Path $taskRoot '.tools\msvc-data'
$env:PORTABLEMSVC_CONFIG = Join-Path $taskRoot '.cache\msvc-config'
$env:PORTABLEMSVC_TEMP = Join-Path $taskRoot '.cache\tmp'
$env:PORTABLEMSVC_MSI_EXTRACTOR = 'pymsi'
$env:PIP_CACHE_DIR = Join-Path $taskRoot '.cache\pip'
$env:PYTHONDONTWRITEBYTECODE = '1'
$env:PATH = "$env:CARGO_HOME\bin;C:\Windows\System32;C:\Windows"
$env:INCLUDE = ''
$env:LIB = ''
$env:LIBPATH = ''
$taskMsvcEnv = Join-Path $taskRoot '.tools\msvc\activate.ps1'
if (Test-Path -LiteralPath $taskMsvcEnv) { . $taskMsvcEnv }
Set-Location -LiteralPath $taskRoot
