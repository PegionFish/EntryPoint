param([int]$Cmd=3)
$src='using System;using System.Runtime.InteropServices;public class WN{'
$src += '[DllImport("user32.dll")]public static extern bool ShowWindowAsync(IntPtr h,int c);'
$src += '[DllImport("user32.dll")]public static extern bool SetForegroundWindow(IntPtr h);'
$src += '[DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();'
$src += '[DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);'
$src += '[DllImport("user32.dll")]public static extern bool AttachThreadInput(uint a,uint b,bool t);'
$src += '[DllImport("kernel32.dll")]public static extern uint GetCurrentThreadId();'
$src += '[DllImport("user32.dll")]public static extern bool SetWindowPos(IntPtr h,IntPtr i,int x,int y,int w,int ht,uint f);'
$src += '}'
Add-Type -TypeDefinition $src
$p = Get-Process entrypoint -ErrorAction Stop | Select-Object -First 1
$h = $p.MainWindowHandle
if ($h -eq 0) { Write-Output "NO_WINDOW"; exit 1 }
[WN]::ShowWindowAsync($h, $Cmd) | Out-Null
$fg = [WN]::GetForegroundWindow()
$fp = [uint32]0
$ft = [WN]::GetWindowThreadProcessId($fg, [ref]$fp)
$mt = [WN]::GetCurrentThreadId()
$att = [WN]::AttachThreadInput($ft, $mt, $true)
$r1 = [WN]::SetForegroundWindow($h)
$top = [IntPtr]::Zero
[WN]::SetWindowPos($h, $top, 0,0,0,0, 0x0003) | Out-Null
[WN]::AttachThreadInput($ft, $mt, $false) | Out-Null
Start-Sleep -Milliseconds 300
$f2 = [WN]::GetForegroundWindow()
Write-Output ("FOREGROUND_MATCH=" + ($f2 -eq $h) + " MAXCMD=" + $Cmd)
