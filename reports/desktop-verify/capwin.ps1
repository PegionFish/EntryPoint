param([string]$Out,[int]$DelayMs=500)
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
$src='using System;using System.Runtime.InteropServices;public class W32{'
$src += '[DllImport("user32.dll")]public static extern IntPtr FindWindow(string c,string t);'
$src += '[DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out RECT r);'
$src += '[StructLayout(LayoutKind.Sequential)]public struct RECT{public int Left,Top,Right,Bottom;}'
$src += '[DllImport("user32.dll")]public static extern bool SetForegroundWindow(IntPtr h);'
$src += '[DllImport("user32.dll")]public static extern bool GetClientRect(IntPtr h,out RECT r);'
$src += '[DllImport("user32.dll")]public static extern IntPtr GetDesktopWindow();'
$src += '}'
Add-Type -TypeDefinition $src
$proc = Get-Process entrypoint -ErrorAction Stop | Select-Object -First 1
$hwnd = $proc.MainWindowHandle
if ($hwnd -eq 0) { Write-Output "NO_WINDOW"; exit 1 }
Add-Type -AssemblyName System.Windows.Forms
$proc | ForEach-Object { $_.MainWindowTitle } | Out-Null
[W32]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds $DelayMs
$r = New-Object W32+RECT
[W32]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
if ($Out) { $bmp.Save($Out) }
$bmp.Dispose(); $g.Dispose()
Write-Output ("CAPTURED " + $w + "x" + $h + " -> " + $Out)
