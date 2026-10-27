# EntryPoint Windows 防火墙规则配置（演进计划·计划2 M3）
#
# 为 WebUI 端口开入站规则（缺省限 Private 配置文件——公共 Wi-Fi 不暴露）；
# -Remove 移除规则。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\configure-firewall.ps1 -Port 9800
#   ... -Profile Any                # 全部配置文件放行（不推荐）
#   ... -Remove

param(
    [int]$Port = 9800,
    [ValidateSet("Private", "Any")] [string]$Profile = "Private",
    [switch]$Remove,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$RuleName = "EntryPoint WebUI $Port"

$profileArg = if ($Profile -eq "Private") { "Profile=private" } else { "Profile=any" }

if ($Remove) {
    $cmd = "netsh advfirewall firewall delete rule name=`"$RuleName`""
    if ($DryRun) { Write-Host "[dry-run] $cmd"; exit 0 }
    cmd /c $cmd
    if ($LASTEXITCODE -ne 0) { throw "remove failed (exit $LASTEXITCODE)" }
    Write-Host "规则已移除：$RuleName"
    exit 0
}

$cmd = "netsh advfirewall firewall add rule name=`"$RuleName`" dir=in action=allow protocol=TCP localport=$Port $profileArg"
if ($DryRun) { Write-Host "[dry-run] $cmd"; exit 0 }
cmd /c $cmd
if ($LASTEXITCODE -ne 0) { throw "add failed (exit $LASTEXITCODE)（需要管理员权限）" }
Write-Host "规则已添加：$RuleName（dir=in TCP $Port，Profile=$Profile）"
