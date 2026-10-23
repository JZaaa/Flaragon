$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
$taskLibrary = Join-Path $taskRoot 'lib'
$taskVendor = Join-Path $taskRoot 'vendor\scintilla-5.6.7'
$taskArchive = Join-Path $taskRoot 'downloads\wscite567-sourceforge.zip'
function Assert-EditorPath([string]$Path) {
    $taskResolved = [IO.Path]::GetFullPath($Path)
    if (-not $taskResolved.StartsWith($taskRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw '下载目标越界' }
    $taskCurrent = $taskResolved
    while ($taskCurrent -and $taskCurrent.StartsWith($taskRoot, [StringComparison]::OrdinalIgnoreCase)) {
        if ((Test-Path -LiteralPath $taskCurrent) -and ((Get-Item -LiteralPath $taskCurrent).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '目标包含重解析点' }
        $taskCurrent = Split-Path -Parent $taskCurrent
    }
}
foreach ($taskDirectory in @($taskLibrary, $taskVendor, (Split-Path $taskArchive), (Join-Path $taskRoot 'licenses'))) {
    Assert-EditorPath $taskDirectory
    New-Item -ItemType Directory -Path $taskDirectory -Force | Out-Null
}
Assert-EditorPath $taskArchive
Assert-EditorPath (Join-Path $taskVendor 'wscite')
if (-not (Test-Path -LiteralPath (Join-Path $taskVendor 'wscite\Scintilla.dll'))) {
    & curl.exe --fail --location --retry 3 --connect-timeout 20 --max-time 300 'https://downloads.sourceforge.net/project/scintilla/SciTE/5.6.7/wscite567.zip' -o $taskArchive
    if ($LASTEXITCODE -ne 0) { throw 'Scintilla 下载失败' }
    Expand-Archive -LiteralPath $taskArchive -DestinationPath $taskVendor -Force
}
foreach ($taskDll in @('Scintilla.dll', 'Lexilla.dll')) {
    $taskSource = Join-Path $taskVendor ('wscite\' + $taskDll)
    Assert-EditorPath $taskSource
    Assert-EditorPath (Join-Path $taskLibrary $taskDll)
    $taskExpected = if ($taskDll -eq 'Scintilla.dll') { '5.6.7' } else { '5.5.4' }
    if ((Get-Item -LiteralPath $taskSource).VersionInfo.FileVersion -ne $taskExpected) { throw "DLL 版本不符：$taskDll" }
    Copy-Item -LiteralPath $taskSource -Destination (Join-Path $taskLibrary $taskDll) -Force
}
Assert-EditorPath (Join-Path $taskVendor 'wscite\License.txt')
Assert-EditorPath (Join-Path $taskRoot 'licenses\Scintilla.txt')
Copy-Item -LiteralPath (Join-Path $taskVendor 'wscite\License.txt') -Destination (Join-Path $taskRoot 'licenses\Scintilla.txt') -Force
Write-Output 'Scintilla 5.6.7 / Lexilla 5.5.4 x64 已准备。'
