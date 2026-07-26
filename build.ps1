#!/usr/bin/env pwsh
# EntryPoint 自动编译打包脚本
# 用法: .\build.ps1 [-Target debug|release] [-SkipTest] [-SkipPackage] [-Clean]

param(
    [ValidateSet("debug", "release")]
    [string]$Target = "release",

    [switch]$SkipTest,
    [switch]$SkipPackage,
    [switch]$Clean,

    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = $PSScriptRoot
$Cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$Rustc = "$env:USERPROFILE\.cargo\bin\rustc.exe"
$Git = "C:\Program Files\Git\bin\git.exe"
$Version = "0.1.0"
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$BuildProfile = if ($Target -eq "release") { "--release" } else { "" }
$ProfileDir = if ($Target -eq "release") { "release" } else { "debug" }

# ── 颜色输出 ──
function Write-Step   { param($m) Write-Host "`n═══ $m ═══" -ForegroundColor Cyan }
function Write-Ok     { param($m) Write-Host "  ✅ $m" -ForegroundColor Green }
function Write-Err    { param($m) Write-Host "  ❌ $m" -ForegroundColor Red }
function Write-Info   { param($m) Write-Host "    $m" -ForegroundColor Yellow }

# ── 工具检查 ──
Write-Step "环境检查"

if (-not (Test-Path $Cargo)) {
    Write-Err "cargo 未找到: $Cargo"
    exit 1
}
Write-Ok "cargo: $Cargo"

if (-not (Test-Path $Git)) {
    Write-Err "git 未找到: $Git"
    exit 1
}
Write-Ok "git: $Git"

$rustcVer = if (Test-Path $Rustc) { & $Rustc --version } else { "unknown" }
Write-Ok "rustc: $rustcVer"

# ── Clean ──
if ($Clean) {
    Write-Step "清理构建产物"
    & $Cargo clean --manifest-path "$ProjectRoot\Cargo.toml"
    Write-Ok "cargo clean 完成"
}

# ── Git 状态 ──
Write-Step "Git 状态"
Set-Location $ProjectRoot
$gitStatus = & $Git status --porcelain
if ($gitStatus) {
    Write-Info "有未提交的改动:"
    $gitStatus | ForEach-Object { Write-Info "  $_" }
} else {
    Write-Ok "工作区干净"
}

$gitHash = & $Git rev-parse --short HEAD
$gitBranch = & $Git rev-parse --abbrev-ref HEAD
Write-Ok "分支: $gitBranch | commit: $gitHash"

# ── Clippy ──
Write-Step "Clippy 检查"
$prevErr = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$clippyOutput = & $Cargo clippy --manifest-path "$ProjectRoot\Cargo.toml" 2>&1
$clippyExit = $LASTEXITCODE
$ErrorActionPreference = $prevErr
if ($clippyExit -ne 0) {
    Write-Err "Clippy 失败"
    $clippyOutput | ForEach-Object { Write-Err "  $_" }
    exit 1
}
$warningCount = ($clippyOutput | Select-String "warning:" | Measure-Object).Count
if ($warningCount -gt 0) {
    Write-Info "Clippy 警告: $warningCount 个"
} else {
    Write-Ok "Clippy 零警告"
}

# ── 测试 ──
if (-not $SkipTest) {
    Write-Step "运行测试"
    $prevErr = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $testOutput = & $Cargo test --manifest-path "$ProjectRoot\Cargo.toml" 2>&1
    $testExit = $LASTEXITCODE
    $ErrorActionPreference = $prevErr
    if ($testExit -ne 0) {
        Write-Err "测试失败"
        $testOutput | Where-Object { $_ -match "FAILED|failures:" } | ForEach-Object { Write-Err "  $_" }
        exit 1
    }
    $passed = ($testOutput | Select-String "test result:" | Select-String "passed" |
        ForEach-Object { if ($_ -match "(\d+) passed") { $matches[1] } }).Count
    Write-Ok "所有测试通过"
} else {
    Write-Info "跳过测试 (-SkipTest)"
}

# ── 编译 ──
Write-Step "编译 ($Target)"
$buildArgs = @("build", "--manifest-path", "$ProjectRoot\Cargo.toml")
if ($Target -eq "release") { $buildArgs += "--release" }
$prevErr = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$buildOutput = & $Cargo @buildArgs 2>&1
$buildExit = $LASTEXITCODE
$ErrorActionPreference = $prevErr
$buildOutputStr = $buildOutput | Out-String
if ($buildExit -ne 0) {
    Write-Err "编译失败"
    $buildOutput | Where-Object { $_ -match "^error" } | ForEach-Object { Write-Err "  $_" }
    exit 1
}
Write-Ok "编译成功"

# ── 打包 ──
if (-not $SkipPackage) {
    Write-Step "打包"

    $distDir = Join-Path $ProjectRoot $OutputDir
    $packageName = "entrypoint-v${Version}-${Target}-${Timestamp}"
    $packageDir = Join-Path $distDir $packageName

    # 创建目录结构
    New-Item -ItemType Directory -Force -Path $packageDir | Out-Null
    New-Item -ItemType Directory -Force -Path "$packageDir\bin" | Out-Null
    New-Item -ItemType Directory -Force -Path "$packageDir\config\pipelines" | Out-Null
    New-Item -ItemType Directory -Force -Path "$packageDir\modules" | Out-Null
    New-Item -ItemType Directory -Force -Path "$packageDir\workspace" | Out-Null
    New-Item -ItemType Directory -Force -Path "$packageDir\webui" | Out-Null

    # 复制二进制
    $binSrc = Join-Path $ProjectRoot "target\$ProfileDir"
    Copy-Item "$binSrc\ep-daemon.exe" "$packageDir\bin\" -Force
    Copy-Item "$binSrc\entrypoint.exe" "$packageDir\bin\" -Force
    Write-Ok "二进制文件已复制"

    # 复制配置
    if (Test-Path "$ProjectRoot\config") {
        Copy-Item "$ProjectRoot\config\*" "$packageDir\config\" -Recurse -Force
        Write-Ok "配置文件已复制"
    }

    # 复制 WebUI 静态文件
    $webuiStatic = Join-Path $ProjectRoot "crates\ep-webui\static"
    if (Test-Path $webuiStatic) {
        Copy-Item "$webuiStatic\*" "$packageDir\webui\" -Recurse -Force
        Write-Ok "WebUI 静态文件已复制"
    }

    # 复制文档
    foreach ($doc in @("README.md", "DESIGN.md", "PROGRESS.md")) {
        $docPath = Join-Path $ProjectRoot $doc
        if (Test-Path $docPath) {
            Copy-Item $docPath "$packageDir\" -Force
        }
    }
    Write-Ok "文档已复制"

    # 生成版本信息
    $versionInfo = @"
EntryPoint $Version
构建时间: $Timestamp
Git 分支: $gitBranch
Git Commit: $gitHash
构建类型: $Target
Rust 版本: $rustcVer
"@
    Set-Content -Path "$packageDir\VERSION.txt" -Value $versionInfo -Encoding UTF8
    Write-Ok "版本信息已生成"

    # 生成启动脚本
    $daemonBat = @"
@echo off
echo Starting EntryPoint Daemon...
bin\ep-daemon.exe
"@
    Set-Content -Path "$packageDir\start-daemon.bat" -Value $daemonBat -Encoding ASCII

    $desktopBat = @"
@echo off
echo Starting EntryPoint Desktop...
bin\entrypoint.exe
"@
    Set-Content -Path "$packageDir\start-desktop.bat" -Value $desktopBat -Encoding ASCII
    Write-Ok "启动脚本已生成"

    # 创建 ZIP 压缩包
    $zipPath = Join-Path $distDir "$packageName.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath }
    Compress-Archive -Path "$packageDir\*" -DestinationPath $zipPath -Force
    $zipSize = [math]::Round((Get-Item $zipPath).Length / 1MB, 1)
    Write-Ok "压缩包已生成: $zipPath ($zipSize MB)"

    # 输出文件清单
    Write-Step "打包清单"
    Get-ChildItem $packageDir -Recurse | ForEach-Object {
        $rel = $_.FullName.Substring($packageDir.Length + 1)
        $size = if ($_.PSIsContainer) { "<dir>" } else { "$([math]::Round($_.Length / 1KB, 1)) KB" }
        Write-Info "  $rel`t$size"
    }

    Write-Ok "打包完成: $zipPath"
} else {
    Write-Info "跳过打包 (-SkipPackage)"
}

# ─ 完成 ──
Write-Step "构建完成"
Write-Ok "版本: $Version | 类型: $Target | Commit: $gitHash | Rust: $rustcVer"
if (-not $SkipPackage) {
    Write-Ok "输出: $zipPath"
}
