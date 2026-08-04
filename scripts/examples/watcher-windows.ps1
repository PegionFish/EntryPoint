# watcher-windows.ps1 — EntryPoint 目录监控 watcher 示例（Windows）
# ============================================================================
# 与 watcher-linux.sh 同款逻辑：FileSystemWatcher 监听新文件落盘
#   → 扩展名过滤 + 短暂静默（防半截文件）+ 幂等检查
#   → POST /api/pipelines/execute
#       { "pipeline_id": "...", "inputs": { "<输入节点>": { "path": "..." } },
#         "wait": false }
#   → 异步提交（wait:false），进度经 WS / 轮询 / callback_url 跟踪
# 演示「管线即 API」的无人值守触发（PACK_UNIFY_PLAN §6.5，决策 6）。
#
# 依赖：Windows PowerShell 5.1+ 或 pwsh 7+（无第三方模块）。
#
# 用法：
#   # 默认：监控 .\watch，提交到管线 video-to-srt（daemon 位于 localhost:9800）
#   powershell -ExecutionPolicy Bypass -File .\watcher-windows.ps1
#
#   # 自定义：
#   powershell -ExecutionPolicy Bypass -File .\watcher-windows.ps1 `
#       -WatchDir D:\incoming -Pipeline video-to-srt -Api http://localhost:9800
#
# 注意（详见 docs/AUTOMATION.md）：
#   - inputs.path 是 daemon 侧的本地路径：watcher 与 daemon 需同机或共享挂载；
#   - API 默认无认证（仅内网 IP 过滤），请只在可信网络内使用；
#   - 模块未运行也无妨：execute 提交路径会自动拉起模块并等健康（§6.5 三件套）；
#   - 并发超额提交自动进入 queued 排队，无需本脚本限流；
#   - Ctrl+C 停止监控。

param(
    # daemon API 地址
    [string]$Api = "http://localhost:9800",
    # 管线 id（config/pipelines/ 内已保存的管线）
    [string]$Pipeline = "video-to-srt",
    # 覆盖 path 的输入节点 id
    [string]$InputNode = "input",
    # 监控目录（不存在则自动创建）
    [string]$WatchDir = ".\watch",
    # 接受的扩展名（逗号分隔）
    [string]$Extensions = ".mp4,.mkv,.mov,.avi,.flac,.wav,.mp3,.m4a",
    # 文件落盘后静默秒数（防半截文件）
    [double]$SettleSecs = 2,
    # 任务终态回调地址（空 = 不使用回调）
    [string]$CallbackUrl = "",
    # 提交成功后写 <文件>.done 标记（防重启重复提交）
    [switch]$MarkDone
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $WatchDir)) { New-Item -ItemType Directory -Path $WatchDir | Out-Null }
$WatchDir = (Resolve-Path $WatchDir).Path
$extSet = @($Extensions -split ',' | ForEach-Object { $_.Trim().ToLowerInvariant() })

function Write-Log([string]$msg) {
    Write-Host "[$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')] $msg"
}

function Submit-File([string]$file) {
    # 与 watcher-linux.sh 相同的请求体：inputs 覆盖 + wait:false 异步
    $body = [ordered]@{
        pipeline_id = $Pipeline
        inputs      = @{ $InputNode = @{ path = $file } }
        wait        = $false
    }
    if ($CallbackUrl) { $body['callback_url'] = $CallbackUrl }
    $json = $body | ConvertTo-Json -Depth 5 -Compress
    try {
        $resp = Invoke-RestMethod -Method Post -Uri "$Api/api/pipelines/execute" `
            -ContentType 'application/json' -Body $json -TimeoutSec 30
        Write-Log "提交成功: $file -> task_id=$($resp.task_id)"
        if ($MarkDone) { New-Item -ItemType File -Path "$file.done" -Force | Out-Null }
    } catch {
        Write-Warning "提交失败: $file（$_）"
    }
}

$watcher = [System.IO.FileSystemWatcher]::new()
$watcher.Path = $WatchDir
$watcher.IncludeSubdirectories = $false
$watcher.InternalBufferSize = 65536
# 事件 → 线程安全队列（Created：新建；Renamed：mv/重命名移入）
$queue = [System.Collections.Concurrent.ConcurrentQueue[string]]::new()
Register-ObjectEvent -InputObject $watcher -EventName Created -SourceIdentifier "EpWatcher.Created" `
    -MessageData $queue -Action { $Event.MessageData.Enqueue($Event.SourceEventArgs.FullPath) } | Out-Null
Register-ObjectEvent -InputObject $watcher -EventName Renamed -SourceIdentifier "EpWatcher.Renamed" `
    -MessageData $queue -Action { $Event.MessageData.Enqueue($Event.SourceEventArgs.FullPath) } | Out-Null
$watcher.EnableRaisingEvents = $true

Write-Log "监控开始: $WatchDir"
Write-Log "管线: $Pipeline（输入节点: $InputNode）→ $Api"
Write-Log "Ctrl+C 停止"

$seen = @{}
try {
    while ($true) {
        [string]$path = $null
        if ($queue.TryDequeue([ref]$path)) {
            if (-not (Test-Path $path -PathType Leaf)) { continue }          # 目录/已删除
            $ext = [System.IO.Path]::GetExtension($path).ToLowerInvariant()
            if ($extSet -notcontains $ext) { continue }                      # 扩展名过滤
            if (Test-Path "$path.done") { continue }                         # 幂等：已提交过
            if ($seen.ContainsKey($path)) { continue }                       # 去重（事件可能成对）
            $seen[$path] = $true
            Start-Sleep -Seconds $SettleSecs                                 # 静默，防半截文件
            Submit-File $path
        } else {
            Start-Sleep -Milliseconds 300
        }
    }
} finally {
    $watcher.EnableRaisingEvents = $false
    Unregister-Event -SourceIdentifier "EpWatcher.Created" -ErrorAction SilentlyContinue
    Unregister-Event -SourceIdentifier "EpWatcher.Renamed" -ErrorAction SilentlyContinue
    $watcher.Dispose()
}
