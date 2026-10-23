param([ValidateSet('visible-stopped','visible-running','tray-running','tray-stopped')][string]$State='visible-stopped',[int]$Seconds=20)
$ErrorActionPreference='Stop'
. "$PSScriptRoot\env.ps1"
$taskGuiId=[int](Get-Content (Join-Path $taskRoot 'testwork/evidence/gui-pid.txt'))
$taskProcess=Get-Process -Id $taskGuiId
if ($taskProcess.Path -ne (Join-Path $taskRoot 'Flaragon.exe')) { throw '测量目标不是交付位置的 Flaragon.exe' }
$taskCpuStart=$taskProcess.TotalProcessorTime.TotalSeconds
$taskWatch=[Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Seconds $Seconds
$taskProcess.Refresh()
$taskElapsed=$taskWatch.Elapsed.TotalSeconds
$taskCpu=($taskProcess.TotalProcessorTime.TotalSeconds-$taskCpuStart)/$taskElapsed*100
$taskResult=[ordered]@{
    version='0.1.0'; state=$State; process_id=$taskGuiId; measurement_seconds=$taskElapsed;
    cpu_one_core_percent=$taskCpu; cpu_machine_percent=($taskCpu/[Environment]::ProcessorCount);
    logical_processors=[Environment]::ProcessorCount; working_set_bytes=$taskProcess.WorkingSet64;
    private_bytes=$taskProcess.PrivateMemorySize64; handles=$taskProcess.HandleCount;
    executable_bytes=(Get-Item -LiteralPath (Join-Path $taskRoot 'Flaragon.exe')).Length;
    os_version=[Environment]::OSVersion.Version.ToString();
    startup='not measured by this script';
    scope='Flaragon manager process only; excludes Apache PHP MySQL Redis; OS file cache not controlled'
}
$taskResult | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $taskRoot "testwork/evidence/performance-$State.json") -Encoding UTF8
$taskResult | ConvertTo-Json -Depth 5
