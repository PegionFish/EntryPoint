#!/usr/bin/env bash
#
# watcher-linux.sh — EntryPoint 目录监控 watcher 示例（Linux）
# ============================================================================
# 监控目录中的新文件落盘，自动提交到 EntryPoint 管线执行 API，
# 演示「管线即 API」的无人值守触发（PACK_UNIFY_PLAN §6.5，决策 6）。
#
# 流程：
#   inotifywait 监听新文件（close_write / moved_to）
#     → 扩展名过滤 + 短暂静默（防半截文件）+ 幂等检查
#     → POST /api/pipelines/execute
#         { "pipeline_id": "...",
#           "inputs": { "<输入节点>": { "path": "<绝对路径>" } },
#           "wait": false }
#     → 异步提交成功（wait:false），进度经 WS / 轮询 / callback_url 跟踪
#
# 依赖：
#   bash、curl、inotify-tools（提供 inotifywait）
#     Debian/Ubuntu:  sudo apt install inotify-tools curl
#     RHEL/Fedora:    sudo dnf install inotify-tools curl
#     Arch:           sudo pacman -S inotify-tools curl
#
# 用法：
#   # 默认：监控 ./watch，提交到管线 video-to-srt（daemon 位于 localhost:9800）
#   ./watcher-linux.sh
#
#   # 自定义（全部可用环境变量覆盖）：
#   EP_WATCH_DIR=/srv/incoming EP_PIPELINE=video-to-srt \
#   EP_API=http://localhost:9800 ./watcher-linux.sh
#
# 环境变量：
#   EP_API           daemon API 地址                （默认 http://localhost:9800）
#   EP_PIPELINE      管线 id（config/pipelines/ 内）（默认 video-to-srt）
#   EP_INPUT_NODE    覆盖 path 的输入节点 id        （默认 input）
#   EP_WATCH_DIR     监控目录，不存在则自动创建      （默认 ./watch）
#   EP_EXTENSIONS    接受的扩展名，逗号分隔          （默认 mp4,mkv,mov,avi,flac,wav,mp3,m4a）
#   EP_SETTLE_SECS   文件落盘后静默秒数，防半截文件  （默认 2）
#   EP_CALLBACK_URL  任务终态回调地址                （默认空 = 不使用回调）
#   EP_MARK_DONE     1 = 提交成功后写 <文件>.done 标记（默认 0；防重启重复提交）
#
# 注意（详见 docs/AUTOMATION.md）：
#   - inputs.path 是 daemon 侧的本地路径：watcher 与 daemon 需同机或共享挂载；
#   - API 默认无认证（仅内网 IP 过滤），请只在可信网络内使用；
#   - 模块未运行也无妨：execute 提交路径会自动拉起模块并等健康（§6.5 三件套）；
#   - 并发超额提交自动进入 queued 排队，无需本脚本限流；
#   - 本脚本只负责「发现 → 提交」；进度/产物跟踪请用 callback_url 或轮询
#     （AUTOMATION.md §3/§4）。
set -euo pipefail

API="${EP_API:-http://localhost:9800}"
PIPELINE="${EP_PIPELINE:-video-to-srt}"
INPUT_NODE="${EP_INPUT_NODE:-input}"
WATCH_DIR="${EP_WATCH_DIR:-./watch}"
EXTENSIONS="${EP_EXTENSIONS:-mp4,mkv,mov,avi,flac,wav,mp3,m4a}"
SETTLE_SECS="${EP_SETTLE_SECS:-2}"
CALLBACK_URL="${EP_CALLBACK_URL:-}"
MARK_DONE="${EP_MARK_DONE:-0}"

command -v inotifywait >/dev/null 2>&1 || {
    echo "缺少依赖: inotifywait（请安装 inotify-tools，详见脚本头部说明）" >&2
    exit 1
}
command -v curl >/dev/null 2>&1 || { echo "缺少依赖: curl" >&2; exit 1; }

mkdir -p "$WATCH_DIR"
WATCH_DIR="$(cd "$WATCH_DIR" && pwd)" # 转绝对路径（inotifywait %w%f 输出依赖）

log() { echo "[$(date '+%F %T')] $*"; }

# JSON 字符串最小转义（反斜杠与双引号）
json_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }

matches_ext() {
    local ext
    ext="$(printf '%s' "${1##*.}" | tr '[:upper:]' '[:lower:]')"
    case ",$EXTENSIONS," in
        *",$ext,"*) return 0 ;;
        *) return 1 ;;
    esac
}

submit() {
    local file="$1" payload resp
    payload=$(printf '{"pipeline_id":"%s","inputs":{"%s":{"path":"%s"}},"wait":false' \
        "$(json_escape "$PIPELINE")" "$(json_escape "$INPUT_NODE")" "$(json_escape "$file")")
    if [ -n "$CALLBACK_URL" ]; then
        payload=$(printf '%s,"callback_url":"%s"}' "$payload" "$(json_escape "$CALLBACK_URL")")
    else
        payload="$payload}"
    fi
    log "提交: $file"
    if resp=$(curl -sS -m 30 -X POST "$API/api/pipelines/execute" \
        -H 'Content-Type: application/json' -d "$payload"); then
        log "提交成功: $resp"
        [ "$MARK_DONE" = "1" ] && touch "$file.done"
    else
        log "提交失败: $file（daemon 不可达或请求被拒；按业务策略可自行重提）" >&2
    fi
}

log "监控开始: $WATCH_DIR"
log "管线: $PIPELINE（输入节点: $INPUT_NODE）→ $API"

# close_write: 本目录内写关闭；moved_to: 从他处 mv 进入
inotifywait -m -q -e close_write -e moved_to --format '%w%f' "$WATCH_DIR" |
    while read -r file; do
        [ -f "$file" ] || continue        # 忽略目录级事件
        matches_ext "$file" || continue   # 扩展名过滤
        [ -f "$file.done" ] && continue   # 幂等：已提交过
        sleep "$SETTLE_SECS"              # 静默等待，防半截文件
        submit "$file"
    done
