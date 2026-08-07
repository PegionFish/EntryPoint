param([int]$DX,[int]$DY)
$src='using System;using System.Runtime.InteropServices;public class CK{'
$src += '[DllImport("user32.dll")]public static extern bool SetCursorPos(int x,int y);'
$src += '[DllImport("user32.dll")]public static extern void mouse_event(uint f,int x,int y,uint d,UIntPtr i);'
$src += '[DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out RECT r);'
$src += '[StructLayout(LayoutKind.Sequential)]public struct RECT{public int Left,Top,Right,Bottom;}'
$src += '}'
Add-Type -TypeDefinition $src
$p = Get-Process entrypoint -ErrorAction Stop | Select-Object -First 1
$h = $p.MainWindowHandle
$r = New-Object CK+RECT
[CK]::GetWindowRect($h, [ref]$r) | Out-Null
