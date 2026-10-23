param([string]$Profile = 'release')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1"
$uiRoot = Join-Path $taskRoot ('testwork\relocation-ui 中文-' + [guid]::NewGuid().ToString('N'))
$evidenceRoot = Join-Path $taskRoot 'testwork\evidence'
$uiExe = Join-Path $taskRoot "target\x86_64-pc-windows-msvc\$Profile\Flaragon.exe"
function Assert-LocalPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $full.StartsWith($taskRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw '验证路径越界' }
    $parts = [Collections.Generic.List[string]]::new()
    $cursor = $full
    while ($cursor) { $parts.Add($cursor); $cursor = [IO.Path]::GetDirectoryName($cursor) }
    $parts.Reverse()
    foreach ($part in $parts) {
        if (Test-Path -LiteralPath $part) {
            if (((Get-Item -LiteralPath $part -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw '拒绝重解析点' }
        }
    }
    return $full
}
$uiRoot = Assert-LocalPath $uiRoot
$null = New-Item -ItemType Directory -Path "$uiRoot\config","$uiRoot\lib",$evidenceRoot -Force
foreach ($dll in @('Scintilla.dll','Lexilla.dll')) {
    Copy-Item -LiteralPath (Assert-LocalPath "$taskRoot\lib\$dll") -Destination "$uiRoot\lib\$dll"
}
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes,System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class RelocationWindow {
  public delegate bool EnumCallback(IntPtr hwnd, IntPtr context);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumCallback callback, IntPtr context);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder text, int length);
  public static IntPtr FindPanel(uint pid) {
    IntPtr result = IntPtr.Zero;
    EnumWindows((hwnd, context) => { uint owner; GetWindowThreadProcessId(hwnd, out owner);
      if (owner != pid) return true;
      var title = new System.Text.StringBuilder(512); GetWindowText(hwnd, title, title.Capacity);
      if (!title.ToString().StartsWith("Flaragon")) return true;
      result = hwnd; return false;
    }, IntPtr.Zero);
    return result;
  }
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int show);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr dc, uint flags);
}
'@
function Launch-Panel {
    Start-Process -FilePath (Assert-LocalPath $uiExe) -ArgumentList ('"' + $uiRoot + '"') -WorkingDirectory $taskRoot -WindowStyle Hidden -PassThru
}
function Panel-Window($Process) {
    $limit = [DateTime]::UtcNow.AddSeconds(25)
    do {
        if ($Process.HasExited) { throw '面板意外退出' }
        $Process.Refresh()
        $handle = [RelocationWindow]::FindPanel([uint32]$Process.Id)
        if ($handle -ne [IntPtr]::Zero) {
            $null = [RelocationWindow]::ShowWindow($handle, 9)
            $null = [RelocationWindow]::SetForegroundWindow($handle)
            Start-Sleep -Milliseconds 300
            return [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $limit)
    throw '未取得主窗口'
}
function Find-Control($Window, [string]$Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $Window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Read-Panel($Window) {
    $elements = $Window.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    ($elements | ForEach-Object { $_.Current.Name }) -join "`n"
}
function Wait-Text($Window, [string]$Text) {
    $limit = [DateTime]::UtcNow.AddSeconds(25)
    do {
        $content = Read-Panel $Window
        if ($content.Contains($Text)) { return $content }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $limit)
    throw "未显示 $Text；窗口内容：$content"
}
function Capture-Panel($Window, [string]$Name) {
    $handle = [IntPtr]$Window.Current.NativeWindowHandle
    $null = [RelocationWindow]::ShowWindow($handle, 9)
    $null = [RelocationWindow]::SetForegroundWindow($handle)
    Start-Sleep -Milliseconds 300
    $rect = [RelocationWindow+Rect]::new()
    $null = [RelocationWindow]::GetWindowRect($handle, [ref]$rect)
    $bitmap = [Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $dc = $graphics.GetHdc()
    try { $null = [RelocationWindow]::PrintWindow($handle, $dc, 2) } finally { $graphics.ReleaseHdc($dc) }
    $bitmap.Save((Assert-LocalPath "$evidenceRoot\$Name.png"), [Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose(); $bitmap.Dispose()
}
function Exit-Panel($Window, $Process) {
    $control = Find-Control $Window '停止并退出'
    if ($null -eq $control) { throw '未找到退出操作' }
    $pattern = $control.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $pattern.Invoke()
    if (-not $Process.WaitForExit(20000)) { throw '退出超时' }
}
$results = [Collections.Generic.List[string]]::new()
[IO.File]::WriteAllText("$uiRoot\config\app.json", '{invalid', [Text.UTF8Encoding]::new($false))
$panel = Launch-Panel
$window = Panel-Window $panel
$null = Wait-Text $window '启动检查失败'
Capture-Panel $window 'relocation-ui-failure'
$duplicate = Launch-Panel
if (-not $duplicate.WaitForExit(15000)) { throw '失败状态下重复启动未退出' }
if ($panel.HasExited) { throw '首个窗口未保持运行' }
$results.Add('启动失败时重复启动恢复已有窗口，不创建第二个常驻进程')
Exit-Panel $window $panel
$results.Add('启动失败状态可停止并退出')
[IO.File]::WriteAllText("$uiRoot\config\app.json", '{"schema":1,"revision":1,"http_port":8080,"apache":null,"default_php":null,"php":[],"mysql":[],"redis":null,"projects":[]}', [Text.UTF8Encoding]::new($false))
$panel = Launch-Panel
$window = Panel-Window $panel
$null = Wait-Text $window '路径检查完成'
Capture-Panel $window 'relocation-ui-ready'
$handle = [IntPtr]$window.Current.NativeWindowHandle
$null = [RelocationWindow]::PostMessage($handle, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)
Start-Sleep -Milliseconds 700
if ($panel.HasExited -or [RelocationWindow]::IsWindowVisible($handle)) { throw '关闭窗口未正确隐藏到托盘' }
$duplicate = Launch-Panel
if (-not $duplicate.WaitForExit(15000)) { throw '重复启动未退出' }
Start-Sleep -Milliseconds 700
if (-not [RelocationWindow]::IsWindowVisible($handle)) { throw '托盘窗口未恢复' }
$results.Add('正常窗口关闭隐藏到托盘，再次启动恢复同一窗口')
Exit-Panel $window $panel
$results.Add('正常状态停止并退出')
@{version='0.2.1'; executable=$uiExe; environment=$uiRoot; results=$results; dpi='当前系统缩放'} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath "$evidenceRoot\relocation-ui.json" -Encoding utf8
$results | ForEach-Object { Write-Output "PASS $_" }
