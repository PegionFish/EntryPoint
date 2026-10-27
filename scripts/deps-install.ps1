# EntryPoint Windows 依赖兜底安装（演进计划·计划2 M3，对标 deploy.sh 依赖段）
#
# 检测 uv（模块 venv/Python 托管核心依赖）；缺失时优先 winget，winget 不可用
# 给出直链指引。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\deps-install.ps1

$ErrorActionPreference = "Stop"

function Test-Command { param([string] $Name)
    [bool] (Get-Command $Name -ErrorAction SilentlyContinue)
}

if (Test-Command uv) {
    $v = (& uv --version) -join " "
    Write-Host "uv 已安装：$v"
    exit 0
}

Write-Host "uv 未安装。"

if (Test-Command winget) {
    Write-Host "尝试 winget 安装（astral-sh.uv）..."
    winget install --id astral-sh.uv --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -eq 0 -and (Test-Command uv)) {
        Write-Host "uv 安装完成（重开终端后 PATH 生效）。"
        exit 0
    }
    Write-Warning "winget 安装未成功（exit $LASTEXITCODE），回退手动指引。"
} else {
    Write-Warning "winget 不可用（应用安装器未安装），回退手动指引。"
}

Write-Host ""
Write-Host "手动安装 uv（任选其一）："
Write-Host "  1) PowerShell 官方脚本："
Write-Host "     powershell -ExecutionPolicy ByPass -c `"irm https://astral.sh/uv/install.ps1 | iex`""
Write-Host "  2) 直链下载 uv-x86_64-pc-windows-msvc.zip 解压到 PATH："
Write-Host "     https://github.com/astral-sh/uv/releases"
Write-Host "（国内网络可用镜像代理后访问；uv 自带 python 托管，无需另装 CPython）"
exit 1
