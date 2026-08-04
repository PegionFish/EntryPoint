#!/usr/bin/env pwsh
# EntryPoint 编译打包脚本（Windows）
# 用法: .\build.ps1 <gui|server> [-Target debug|release] [-SkipTest] [-SkipClippy] [-Clean] [-OutputDir <dir>]
#   gui    — 桌面 GUI 客户端包（zip：entrypoint.exe + 配置 + 模块，解压即用）
#   server — 服务器包（zip：ep-daemon + WebUI 静态资源 + 配置 + 模块）

param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet("gui", "server")]
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

$Version = "0.1.0"
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$ProfileDir = if ($Target -eq "release") { "release" } else { "debug" }
$CrateName = if ($Mode -eq "gui") { "ep-desktop" } else { "ep-daemon" }
$PackageBase = "EntryPoint-v${Version}-win64-${Mode}"

function Write-Step { param($m) Write-Host "`n=== $m ===" -ForegroundColor Cyan }
function Write-Ok   { param($m) Write-Host "  [OK] $m" -ForegroundColor Green }
function Write-Err  { param($m) Write-Host "  [FAIL] $m" -ForegroundColor Red; exit 1 }
function Write-Info { param($m) Write-Host "  $m" -ForegroundColor Yellow }

# ── 1. 环境检查 ──
Write-Step "环境检查"
Write-Ok "cargo: $Cargo"
Write-Ok "rustc: $(& $Rustc --version)"
if ($Git) { Write-Ok "git: $Git" } else { Write-Info "git 未找到，跳过版本信息" }

$gitHash = if ($Git) { (& $Git rev-parse --short HEAD).Trim() } else { "unknown" }
$gitBranch = if ($Git) { (& $Git rev-parse --abbrev-ref HEAD).Trim() } else { "unknown" }
$rustcVer = & $Rustc --version

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
    $failCount = ($testOutput | Select-String "test result:" | Select-String "failed" |
        ForEach-Object { if ($_ -match "(\d+) failed") { [int]$matches[1] } }).Count
    if ($failCount -gt 0) { Write-Err "测试失败: $failCount 个" }
    Write-Ok "所有测试通过"
} else {
    Write-Info "跳过测试 (-SkipTest)"
}

# ── 5. 编译 ──
Write-Step "编译 ($Target) — $CrateName"
$buildArgs = @("build", "--manifest-path", "$ProjectRoot\Cargo.toml", "-p", $CrateName)
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
$exeName = if ($Mode -eq "gui") { "entrypoint.exe" } else { "ep-daemon.exe" }
if (-not (Test-Path "$binSrc\$exeName")) { Write-Err "二进制不存在: $binSrc\$exeName（请先编译）" }
Copy-Item "$binSrc\$exeName" "$packageDir\bin\" -Force
Write-Ok "二进制: bin\$exeName"

# VC 运行库（免装 VC++ Redistributable）
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

# 配置
Copy-Item "$ProjectRoot\config\app.toml" "$packageDir\config\" -Force
Copy-Item "$ProjectRoot\config\pipelines\*" "$packageDir\config\pipelines\" -Force
Write-Ok "config\ 已复制"

# 模块
New-Item -ItemType Directory -Force -Path "$packageDir\modules" | Out-Null
Get-ChildItem "$ProjectRoot\modules" -Directory | ForEach-Object {
    Copy-Item $_.FullName "$packageDir\modules\" -Recurse -Force
    Remove-Item "$packageDir\modules\$($_.Name)\__pycache__" -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Ok "modules\ 已复制"

# 服务器包：WebUI 静态资源 → webui\
if ($Mode -eq "server") {
    $webuiStatic = Join-Path $ProjectRoot "crates\ep-webui\static"
    if (Test-Path $webuiStatic) {
        New-Item -ItemType Directory -Force -Path "$packageDir\webui" | Out-Null
        Copy-Item "$webuiStatic\*" "$packageDir\webui\" -Recurse -Force
        Write-Ok "webui\ 已复制"
    } else {
        Write-Info "警告: crates\ep-webui\static 不存在（请先构建 WebUI 前端）"
    }
}

# 启动脚本
if ($Mode -eq "gui") {
    $launcher = "@echo off`r`ncd /d `"%~dp0`"`r`nstart `"`" bin\entrypoint.exe`r`n"
    Set-Content -Path "$packageDir\start-desktop.bat" -Value $launcher -Encoding ASCII
} else {
    $launcher = "@echo off`r`ncd /d `"%~dp0`"`r`nbin\ep-daemon.exe`r`n"
    Set-Content -Path "$packageDir\start-daemon.bat" -Value $launcher -Encoding ASCII
}
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
