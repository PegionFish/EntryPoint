#!/usr/bin/env bash
# scripts/verify-api.sh — API 端点可用性验证（docs/PLAN_WAIFU2X_MODULE.md W3 §5）
#
# 用法：
#   ./scripts/verify-api.sh [BASE_URL] [--with-module <module_id>] [--image <path>]
#
# 默认 BASE_URL=http://127.0.0.1:9800。--with-module 指定真实模块做推理闭环
# （如 waifu2x：启动模块 → v1 提交 → 轮询 → 产物下载），缺省只验平台只读面
# 与 run 资源语义。退出码 0 = 全部通过；任一失败立即退出并打印已通过序号。
#
# 依赖：curl、python3（JSON 解析）。鉴权：EP_TOKEN 环境变量（可选）。

set -euo pipefail

BASE="${1:-http://127.0.0.1:9800}"
MODULE=""
IMAGE=""
shift_args=("$@")
if [[ "${2:-}" == "--with-module" ]]; then MODULE="${3:-}"; fi
if [[ "${4:-}" == "--image" ]]; then IMAGE="${5:-}"; fi

AUTH=()
[[ -n "${EP_TOKEN:-}" ]] && AUTH=(-H "Authorization: Bearer ${EP_TOKEN}")

PASS=0
step() { printf '\n[%02d] %s\n' "$((PASS + 1))" "$*"; }
ok()   { printf '     ✓ %s\n' "$*"; PASS=$((PASS + 1)); }
die()  { printf '     ✗ %s\n' "$*" >&2; exit 1; }

jq_get() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }

req() { # method path [curl-extra...]
    local m="$1" p="$2"; shift 2
    curl -sS -X "$m" "${AUTH[@]}" -H "Content-Type: application/json" "$@" "${BASE}${p}"
}

# ── 平台只读面 ──────────────────────────────────────────────────────────────

step "GET /api/health"
H=$(req GET /api/health)
echo "$H" | grep -q '"status"' || die "health 响应异常: $H"
ok "$H"

step "GET /api/devices（canonical 只读面）"
req GET /api/devices | grep -q '"' && ok "设备列表可读" || die "devices 失败"

step "GET /api/modules（模块发现）"
MODS=$(req GET /api/modules)
ok "$(echo "$MODS" | jq_get "len(d.get('modules', d) if isinstance(d, dict) else d)" 2>/dev/null || echo "可解析") 条模块"

step "GET /api/v1/capabilities（v1 稳定契约）"
CAPS=$(req GET /api/v1/capabilities)
echo "$CAPS" | grep -q '"capabilit' || die "v1 capabilities 异常: $(echo "$CAPS" | head -c 200)"
ok "能力目录可读"

step "GET /api/v1/tasks（v1 任务列表）"
req GET /api/v1/tasks | grep -q '"tasks"' && ok "任务列表可读" || die "v1 tasks 异常"

step "v1 未知任务 → 404 + 机读码"
CODE=$(req GET /api/v1/tasks/__no_such_task__ | jq_get "d['error']['code']")
[[ "$CODE" == "TASK_NOT_FOUND" ]] && ok "error.code=TASK_NOT_FOUND" || die "got $CODE"

step "旧路径别名抽查（/api/models/{id} → 200/4xx 非 404-fallback）"
S=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH[@]}" "${BASE}/api/models/__probe__")
[[ "$S" != "404" ]] && ok "HTTP $S（别名命中 handler）" || die "别名疑似丢失（404）"

# ── run 资源语义（集合级创建的 400 校验路径） ────────────────────────────────

step "POST /api/pipelines/runs（{} 双缺 → 400，证明路由+校验活）"
S=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" \
    -H 'Content-Type: application/json' -d '{}' "${BASE}/api/pipelines/runs")
[[ "$S" == "400" ]] && ok "HTTP 400" || die "HTTP $S"

step "POST /api/pipelines/{id}/runs（不存在管线 → 404）"
S=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" \
    -H 'Content-Type: application/json' -d '{"pipeline_id":"__no_such__"}' \
    "${BASE}/api/pipelines/__no_such__/runs")
[[ "$S" == "404" ]] && ok "HTTP 404" || die "HTTP $S"

# ── 真实模块推理闭环（可选） ────────────────────────────────────────────────

if [[ -n "$MODULE" ]]; then
    step "POST /api/modules/$MODULE/start"
    req POST "/api/modules/$MODULE/start" | grep -q '"' && ok "启动指令受理" || die "启动失败"

    step "轮询 /api/modules/$MODULE/status 直到 Running（≤600s）"
    for _ in $(seq 1 120); do
        ST=$(req GET "/api/modules/$MODULE/status" | jq_get "d.get('status','')" | tr 'A-Z' 'a-z')
        [[ "$ST" == "running" ]] && break
        sleep 5
    done
    [[ "$ST" == "running" ]] && ok "Running" || die "状态=$ST"

    step "GET /api/v1/capabilities 含 $MODULE"
    echo "$CAPS" | grep -q "$MODULE" && ok "能力已聚合" || die "v1 目录未见 $MODULE"

    if [[ -n "$IMAGE" ]]; then
        CAP="${CAP_OVERRIDE:-upscale_image}"
        step "POST /api/files 上传输入 $IMAGE"
        UP=$(curl -sS -X POST "${AUTH[@]}" -F "file=@${IMAGE}" "${BASE}/api/files")
        IN_PATH=$(echo "$UP" | jq_get "d['path']")
        [[ -n "$IN_PATH" ]] && ok "staging: $IN_PATH" || die "上传失败: $UP"

        step "POST /api/v1/inference/$MODULE/$CAP（input_path 引用）"
        SUB=$(req POST "/api/v1/inference/$MODULE/$CAP" \
            -d "{\"input_path\": \"$IN_PATH\", \"params\": {\"method\": \"noise_scale\", \"noise_level\": 1}}")
        TASK=$(echo "$SUB" | jq_get "d['task_id']")
        [[ -n "$TASK" ]] && ok "task_id=$TASK" || die "提交失败: $SUB"

        step "轮询 GET /api/v1/tasks/$TASK（≤30min）"
        for _ in $(seq 1 120); do
            R=$(req GET "/api/v1/tasks/$TASK")
            ST=$(echo "$R" | jq_get "d['status']")
            case "$ST" in
                completed|failed|cancelled) break ;;
            esac
            sleep 15
        done
        [[ "$ST" == "completed" ]] && ok "completed" || die "终态=$ST: $R"

        step "产物下载"
        URL=$(echo "$R" | jq_get "d['outputs'][0]['url']")
        OUT="/tmp/ep-verify-artifact-$(basename "$URL")"
        curl -sS -o "$OUT" "${BASE}${URL}"
        [[ -s "$OUT" ]] && ok "产物 $(wc -c < "$OUT") 字节 → $OUT" || die "下载失败"

        step "POST /api/v1/tasks/$TASK/cancel（已终态 → 409）"
        S=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" "${BASE}/api/v1/tasks/$TASK/cancel")
        [[ "$S" == "409" ]] && ok "HTTP 409 TASK_ALREADY_TERMINAL" || die "HTTP $S"
    else
        printf '     （未提供 --image，跳过推理闭环）\n'
    fi

    step "POST /api/modules/$MODULE/stop"
    req POST "/api/modules/$MODULE/stop" | grep -q '"' && ok "已停止" || die "停止失败"
fi

printf '\n═══ 全部通过：%d 项 ═══\n' "$PASS"
