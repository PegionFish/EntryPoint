# EntryPoint Windows 服务安装脚本（演进计划·计划2 M3）
#
# 以 sc create 直接托管 ep-daemon --service（daemon 侧经 windows-service 与 SCM
# 对话，见 crates/ep-daemon/src/service.rs；服务进程不与 SCM 通信 ~30s 被杀）。
#
# 用法示例：
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-service.ps1 `
#     -BinDir C:\EntryPoint\bin
#   ... -Account NetworkService          # 建议无人值守场景用低权限账户
#   ... -Uninstall                        # 停止并移除服务（不动 workspace/config）
#   ... -DryRun                           # 只打印将执行的命令
#
# 权限注意：服务账户（默认 LocalSystem）必须能读写工作目录；用专用账户时先
# icacls 授权（脚本会提示命令），否则启动即因配置/工作区不可写而退出。

param(
    [Parameter(Mandatory)] [string]$BinDir,
    [int]$Port = 9800,
    [string]$ServiceName = "EntryPoint",
    [ValidateSet("LocalSystem", "NetworkService")] [string]$Account = "LocalSystem",
    [switch]$Uninstall,
    [switch]$Force,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$ExePath = Join-Path $BinDir "ep-daemon.exe"

function Invoke-Plan {
    param([string[]] $Commands, [switch] $Execute)
    foreach ($cmd in $Commands) {
        if ($Execute) {
            Write-Host "> $cmd"
            cmd /c $cmd
            if ($LASTEXITCODE -ne 0) { throw "command failed (exit $LASTEXITCODE): $cmd" }
        } else {
            Write-Host "[dry-run] $cmd"
        }
    }
}

if (-not (Test-Path $ExePath)) {
    throw "ep-daemon.exe not found at: $ExePath（先 build.ps1 或 cargo build --release）"
}

if ($Uninstall) {
    $plan = @(
        "sc.exe stop $ServiceName",
        "sc.exe delete $ServiceName"
    )
    Invoke-Plan -Commands $plan -Execute:(-not $DryRun)
    Write-Host "服务已移除（workspace/config 未动）。"
    exit 0
}

if (-not $Force) {
    $existing = sc.exe query $ServiceName 2>$null
    if ($LASTEXITCODE -eq 0) {
        throw "服务 $ServiceName 已存在；覆盖请加 -Force（先 stop+delete 再装）"
    }
}

# binPath 必须带引号（BinDir 含空格）并显式 --service 参数
$binPath = "`"$ExePath`" --service"
$objArg = if ($Account -eq "NetworkService") { "obj= NetworkService password= `"`"" } else { "" }

$plan = @()
if ($Force) {
    $plan += "sc.exe stop $ServiceName"
    $plan += "sc.exe delete $ServiceName"
}
$plan += @(
    "sc.exe create $ServiceName binPath= `"$binPath`" start= auto DisplayName= `"EntryPoint Daemon`" $objArg",
    "sc.exe description $ServiceName `"EntryPoint daemon (WebUI http://127.0.0.1:$Port)`"",
    "sc.exe failure $ServiceName reset= 86400 actions= restart/10000/restart/30000/restart/60000",
    "sc.exe start $ServiceName"
)

Invoke-Plan -Commands $plan -Execute:(-not $DryRun)

if (-not $DryRun) {
    Write-Host ""
    Write-Host "服务已安装并启动：WebUI http://127.0.0.1:$Port"
    if ($Account -eq "NetworkService") {
        Write-Host "提醒：NetworkService 需对工作目录可写，必要时执行："
        Write-Host "  icacls `<workspace`> /grant `"NT AUTHORITY\NETWORK SERVICE`:(OI)(CI)M`" /T"
    }
    Write-Host "卸载：powershell -File scripts\install-service.ps1 -BinDir $BinDir -Uninstall"
}
