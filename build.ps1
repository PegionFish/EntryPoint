#!/usr/bin/env pwsh
# EntryPoint 编译打包脚本（Windows）— server 模式（桌面端已于 2026-08-13 退役）
# 用法: .\build.ps1 server [-Target debug|release] [-SkipTest] [-SkipClippy] [-Clean] [-OutputDir <dir>]
#   server — 服务器包（zip：ep-daemon + WebUI 静态资源 + 配置 + 模块）

param(
    [Parameter(Mandatory, Position = 0)]
    [string]$Mode,

    [ValidateSet("debug", "release")]
    [string]$Target = "release",

    [switch]$SkipTest,
    [switch]$SkipClippy,
    [switch]$Clean,

    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = $PSScriptRoot

# 桌面端退役迁移提示（2026-08-13）：gui 模式不再提供，明确提示后以非零码退出
if ($Mode -eq "gui") {
    Write-Host "`n[FAIL] gui 模式已随桌面端退役（2026-08-13）。" -ForegroundColor Red
    Write-Host "  WebUI 为唯一 UI，请改用 server 模式构建：" -ForegroundColor Yellow
    Write-Host "    .\build.ps1 server" -ForegroundColor Cyan
    Write-Host "  历史说明见 docs/DESKTOP_SUNSET_PLAN.md。`n" -ForegroundColor Yellow
    exit 1
}
if ($Mode -ne "server") {
    Write-Host "`n[FAIL] 未知模式: $Mode（仅支持 server）`n" -ForegroundColor Red
    exit 1
}

# ── 工具查找 ──
$Cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
if (-not (Test-Path $Cargo)) {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) { $Cargo = $cmd.Source } else { Write-Error "cargo 未找到，请先安装 Rust: https://rustup.rs"; exit 1 }
}
$Rustc = "$env:USERPROFILE\.cargo\bin\rustc.exe"
if (-not (Test-Path $Rustc)) {
    $cmd = Get-Command rustc -ErrorAction SilentlyContinue
    if ($cmd) { $Rustc = $cmd.Source } else { $Rustc = "unknown" }
}
$Git = Get-Command git -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source

# 版本单一来源：Cargo.toml [workspace.package] version（勿在此另写死版本号）；
# 运行时二进制经 env!("CARGO_PKG_VERSION") 同源，保证包名/VERSION.txt/版本显示一致。
# 解析失败必须显式报错而非回退 0.0.0（否则静默产出错误命名的包）。
$VersionLine = Select-String -Path "$ProjectRoot\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' -ErrorAction SilentlyContinue | Select-Object -First 1
$Version = if ($VersionLine) { $VersionLine.Matches[0].Groups[1].Value } else { "" }
if ($Version -notmatch '^\d+\.\d+\.\d+') { Write-Error "无法从 Cargo.toml 解析版本号，拒绝打包（请检查 [workspace.package] version）"; exit 1 }
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$ProfileDir = if ($Target -eq "release") { "release" } else { "debug" }
$CrateName = "ep-daemon"
$PackageBase = "EntryPoint-v${Version}-win64-${Mode}"

function Write-Step { param($m) Write-Host "`n=== $m ===" -ForegroundColor Cyan }
function Write-Ok   { param($m) Write-Host "  [OK] $m" -ForegroundColor Green }
function Write-Err  { param($m) Write-Host "  [FAIL] $m" -ForegroundColor Red; exit 1 }
function Write-Info { param($m) Write-Host "  $m" -ForegroundColor Yellow }

# ── 1. 环境检查 ──
Write-Step "环境检查"
Write-Ok "cargo: $Cargo"
# rustc 未找到时 $Rustc 为 "unknown"，先判再调用（否则 & "unknown" 在 Stop 模式下直接崩溃）
if ($Rustc -ne "unknown") {
    $rustcVer = & $Rustc --version
    Write-Ok "rustc: $rustcVer"
} else {
    Write-Info "rustc 未找到，跳过版本信息"
    $rustcVer = "unknown"
}
if ($Git) { Write-Ok "git: $Git" } else { Write-Info "git 未找到，跳过版本信息" }

$gitHash = if ($Git) { (& $Git rev-parse --short HEAD).Trim() } else { "unknown" }
$gitBranch = if ($Git) { (& $Git rev-parse --abbrev-ref HEAD).Trim() } else { "unknown" }

# ── 2. Clean ──
if ($Clean) {
    Write-Step "清理构建产物"
    & $Cargo clean --manifest-path "$ProjectRoot\Cargo.toml"
    Write-Ok "cargo clean 完成"
}

# ── 3. Clippy ──
if (-not $SkipClippy) {
    Write-Step "Clippy 检查"
    $prevErr = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $clippyOutput = & $Cargo clippy --manifest-path "$ProjectRoot\Cargo.toml" --workspace --all-targets 2>&1
    $clippyExit = $LASTEXITCODE
    $ErrorActionPreference = $prevErr
    if ($clippyExit -ne 0) {
        $clippyOutput | Where-Object { $_ -match "^(warning:|error(\[|:))" } | ForEach-Object { Write-Host "  [FAIL] $_" -ForegroundColor Red }
        Write-Err "Clippy 失败"
    }
    $warnCount = ($clippyOutput | Select-String "warning:" | Measure-Object).Count
    if ($warnCount -gt 0) { Write-Info "Clippy 警告: $warnCount 个" } else { Write-Ok "Clippy 零警告" }
} else {
    Write-Info "跳过 Clippy (-SkipClippy)"
}

# ── 4. 测试 ──
if (-not $SkipTest) {
    Write-Step "运行测试"
    $prevErr = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $testOutput = & $Cargo test --manifest-path "$ProjectRoot\Cargo.toml" --workspace 2>&1
    $testExit = $LASTEXITCODE
    $ErrorActionPreference = $prevErr
    if ($testExit -ne 0) {
        $testOutput | Where-Object { $_ -match "FAILED|failures:|error\[" } | ForEach-Object { Write-Host "  [FAIL] $_" -ForegroundColor Red }
        Write-Err "测试失败"
    }
    # 修复：原实现把「摘要行含 failed 字样的套件数」误计为失败测试数——
    # cargo 每条 `test result:` 摘要恒含 "N failed"（全绿时为 "0 failed"），
    # 导致全绿也必定误报「测试失败: <套件总数> 个」（本工作区为 18）。
    # 现改为对各套件摘要的真实失败数求和；全绿时为 0。
    $failCount = ($testOutput | Select-String "test result:" |
        ForEach-Object { if ($_ -match "(\d+) failed") { [int]$matches[1] } else { 0 } } |
        Measure-Object -Sum).Sum
    if ($failCount -gt 0) {
        $testOutput | Where-Object { $_ -match "FAILED|failures:" } | ForEach-Object { Write-Host "  [FAIL] $_" -ForegroundColor Red }
        Write-Err "测试失败: $failCount 个"
    }
    Write-Ok "所有测试通过"
} else {
    Write-Info "跳过测试 (-SkipTest)"
}

# ── 5. 编译 ──
# 仲裁 #36：ep-pack-cli（bin 名 ep-pack）随主 crate 一并构建并纳入打包
Write-Step "编译 ($Target) — $CrateName + ep-pack-cli"
$buildArgs = @("build", "--manifest-path", "$ProjectRoot\Cargo.toml", "-p", $CrateName, "-p", "ep-pack-cli")
if ($Target -eq "release") { $buildArgs += "--release" }
$prevErr = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$buildOutput = & $Cargo @buildArgs 2>&1
$buildExit = $LASTEXITCODE
$ErrorActionPreference = $prevErr
if ($buildExit -ne 0) {
    $buildOutput | Where-Object { $_ -match "^error" } | ForEach-Object { Write-Host "  [FAIL] $_" -ForegroundColor Red }
    Write-Err "编译失败"
}
Write-Ok "编译成功"

# ── 6. 打包 ──
Write-Step "打包 ($Mode)"

$distDir = Join-Path $ProjectRoot $OutputDir
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
$packageDir = Join-Path $distDir $PackageBase
if (Test-Path $packageDir) { Remove-Item $packageDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path "$packageDir\bin" | Out-Null
New-Item -ItemType Directory -Force -Path "$packageDir\config\pipelines" | Out-Null
New-Item -ItemType Directory -Force -Path "$packageDir\workspace" | Out-Null

$binSrc = Join-Path $ProjectRoot "target\$ProfileDir"
$exeName = "ep-daemon.exe"
if (-not (Test-Path "$binSrc\$exeName")) { Write-Err "二进制不存在: $binSrc\$exeName（请先编译）" }
Copy-Item "$binSrc\$exeName" "$packageDir\bin\" -Force
Write-Ok "二进制: bin\$exeName"

# ep-pack CLI（仲裁 #36：server 包附带，bin 名 ep-pack）
if (-not (Test-Path "$binSrc\ep-pack.exe")) { Write-Err "ep-pack 二进制不存在: $binSrc\ep-pack.exe（请先编译）" }
Copy-Item "$binSrc\ep-pack.exe" "$packageDir\bin\" -Force
Write-Ok "ep-pack CLI: bin\ep-pack.exe"

# VC 运行库（免装 VC++ Redistributable）
# 说明（§3.1/§15.3）：Windows 侧共享 CUDA 库目录 runtime\cuda-libs 的 PATH 前置
# 由 daemon 运行时代码处理（ep-core process.rs 按 DLL 搜索序注入模块子进程），
# 打包脚本无需在此注入 PATH；若存在 runtime\cuda-libs 则按存在性随包附带（见下）。
$crtDir = Get-ChildItem "C:\Program Files\Microsoft Visual Studio" -Recurse -Directory -Filter "Microsoft.VC14*.CRT" -ErrorAction SilentlyContinue |
    Where-Object { $_.Parent.Name -eq "x64" -and $_.Parent.Parent.Name -match "^\d+\.\d+" } |
    ForEach-Object { [PSCustomObject]@{ Path = $_.FullName; Ver = [version]$_.Parent.Parent.Name } } |
    Sort-Object Ver -Descending | Select-Object -First 1
if ($crtDir) {
    foreach ($dll in @("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll", "msvcp140_atomic_wait.dll", "concrt140.dll")) {
        Copy-Item "$($crtDir.Path)\$dll" "$packageDir\bin\" -Force -ErrorAction SilentlyContinue
    }
    Write-Ok "VC 运行库已附带 (version $($crtDir.Ver))"
} else {
    Write-Info "未找到 VS Redist 目录，跳过 VC 运行库（需目标机已装 VC++ 运行库）"
}

# 配置（整目录复制，与 build.sh 的 cp -a config/. 等价——
# constraints.txt 等后续新增文件自动包含，避免双平台漂移）
Copy-Item "$ProjectRoot\config\*" "$packageDir\config\" -Recurse -Force
Write-Ok "config\ 已复制"

# 模块
New-Item -ItemType Directory -Force -Path "$packageDir\modules" | Out-Null
Get-ChildItem "$ProjectRoot\modules" -Directory | ForEach-Object {
    Copy-Item $_.FullName "$packageDir\modules\" -Recurse -Force
    Remove-Item "$packageDir\modules\$($_.Name)\__pycache__" -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Ok "modules\ 已复制"

# 共享 CUDA 库目录（§3.1）：可选资产，存在才随包附带（缺失不报错；
# .gitignore 忽略 runtime/，由部署者自备）。PATH 前置由 daemon 代码处理（见上注）。
$cudaLibs = Join-Path $ProjectRoot "runtime\cuda-libs"
if (Test-Path $cudaLibs) {
    New-Item -ItemType Directory -Force -Path "$packageDir\runtime" | Out-Null
    Copy-Item $cudaLibs "$packageDir\runtime\cuda-libs" -Recurse -Force
    Write-Ok "runtime\cuda-libs 已随包附带（可选目录）"
} else {
    Write-Info "runtime\cuda-libs 不存在，跳过（可选目录）"
}

# WebUI 静态资源 → webui\
$webuiStatic = Join-Path $ProjectRoot "crates\ep-webui\static"
if (Test-Path $webuiStatic) {
    New-Item -ItemType Directory -Force -Path "$packageDir\webui" | Out-Null
    Copy-Item "$webuiStatic\*" "$packageDir\webui\" -Recurse -Force
    Write-Ok "webui\ 已复制"
} else {
    Write-Info "警告: crates\ep-webui\static 不存在（请先构建 WebUI 前端）"
}

# 启动脚本
# 启动体验（C2/§3）：双击 → 独立控制台拉起 daemon → 延迟后自动开默认浏览器；
# 传 --no-browser 跳过开浏览器（无人值守/远程部署场景）
$launcher = @'
@echo off
rem EntryPoint daemon launcher — 双击启动，随后自动打开浏览器
rem 用法: start-daemon.bat [--no-browser]
cd /d "%~dp0"
set "OPEN_BROWSER=1"
if /i "%~1"=="--no-browser" set "OPEN_BROWSER=0"
start "EntryPoint Daemon" bin\ep-daemon.exe
if "%OPEN_BROWSER%"=="1" (
    timeout /t 2 /nobreak >nul
    start "" http://127.0.0.1:9800
)
'@
Set-Content -Path "$packageDir\start-daemon.bat" -Value $launcher -Encoding ASCII
Write-Ok "启动脚本已生成"

# 文档 + 版本信息
if (Test-Path "$ProjectRoot\README.md") { Copy-Item "$ProjectRoot\README.md" "$packageDir\" -Force }
$versionInfo = @"
EntryPoint $Version ($Mode, Windows x64)
构建时间: $Timestamp
Git 分支: $gitBranch
Git Commit: $gitHash
构建类型: $Target
Rust 版本: $rustcVer
"@
Set-Content -Path "$packageDir\VERSION.txt" -Value $versionInfo -Encoding UTF8
Write-Ok "VERSION.txt 已生成"

# 压缩
$zipPath = Join-Path $distDir "$PackageBase.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath }
Compress-Archive -Path "$packageDir\*" -DestinationPath $zipPath -Force -CompressionLevel Optimal
$zipSize = [math]::Round((Get-Item $zipPath).Length / 1MB, 2)
Write-Ok "压缩包: $zipPath ($zipSize MB)"

Write-Step "打包清单"
Get-ChildItem $packageDir -Recurse | ForEach-Object {
    $rel = $_.FullName.Substring($packageDir.Length + 1)
    $size = if ($_.PSIsContainer) { "<dir>" } else { "$([math]::Round($_.Length / 1KB, 1)) KB" }
    Write-Info "  $rel`t$size"
}

Write-Step "完成"
Write-Ok "输出: $zipPath"
