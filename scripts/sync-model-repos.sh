#!/usr/bin/env bash
# ============================================================================
# sync-model-repos.sh — 将主仓库 modules/ 适配层反向同步到 EntryPoint_Models/
#
# 用途：保持 /home/bob/EntryPoint_Models/<repo>/（独立 git 仓库）与主仓库同步，
#       每个 repo 可分别推送到 GitHub（origin 由用户自建）。
#
# repo 布局（设计约定）：
#   * 独占 repo（如 faster-whisper/）：仓库根 = module 文件 + README.md(用法文档)
#     + ADAPTER.md(反向同步总览) + upstream.json(血缘元数据)
#   * 多适配器 repo（Real-ESRGAN/）：README.md(总览) + adapters/<module_id>/
#
# 用法:
#   ./scripts/sync-model-repos.sh              # 全量同步 11 个模块（10 个 repo）
#   ./scripts/sync-model-repos.sh --only birefnet
#   ./scripts/sync-model-repos.sh --push       # 同步后推送到各 repo 的 origin
#
# 约定：主仓库 modules/ 是唯一事实源；反向 repo 不应直接编辑 module 文件。
# ============================================================================
set -euo pipefail

EP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODELS_ROOT="${EP_MODELS_ROOT:-/home/bob/EntryPoint_Models}"
EP_HEAD="$(git -C "$EP_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
SINCE="$(date +%Y-%m-%d)"
ONLY=""
PUSH=0

# module_id -> "GitHubRepoName|UpstreamUrl|模块名"
# * GitHubRepoName 统一以 EntryPoint_ 前缀命名（明确标注是 EntryPoint 插件）
# * Real-ESRGAN 一个 repo 收纳多个 module（适配子目录）
declare -A MOD_TO_REPO=(
  [birefnet]="EntryPoint_BiRefNet|https://github.com/ZhengPeng7/BiRefNet|BiRefNet 图像抠图"
  [deep-filter]="EntryPoint_DeepFilterNet|https://github.com/Rikorose/DeepFilterNet|DeepFilter 音频降噪"
  [faster-whisper]="EntryPoint_faster-whisper|https://github.com/SYSTRAN/faster-whisper|Faster-Whisper ASR"
  [firered-ocr]="EntryPoint_FireRed-OCR|https://github.com/FireRedTeam/FireRed-OCR|FireRed-OCR 文档识别"
  [paddleocr]="EntryPoint_PaddleOCR|https://github.com/PaddlePaddle/PaddleOCR|PaddleOCR 文字识别"
  [qwen3-asr]="EntryPoint_Qwen3-ASR|https://github.com/QwenLM/Qwen3-ASR|Qwen3-ASR 语音识别"
  [qwen3-tts]="EntryPoint_Qwen3-TTS|https://github.com/QwenLM/Qwen3-TTS|Qwen3-TTS 语音合成"
  [animevideo]="EntryPoint_Real-ESRGAN|https://github.com/xinntao/Real-ESRGAN|AnimeVideo 视频超分"
  [realesr]="EntryPoint_Real-ESRGAN|https://github.com/xinntao/Real-ESRGAN|RealESR 视频超分"
  [rembg]="EntryPoint_rembg|https://github.com/danielgatis/rembg|RemBG 智能去背景"
  [rife]="EntryPoint_rife-ncnn-vulkan|https://github.com/nihui/rife-ncnn-vulkan|RIFE 视频插帧"
)

log() { printf '[sync] %s\n' "$*"; }
die() { printf '[sync][失败] %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --only) ONLY="$2"; shift 2 ;;
    --push) PUSH=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) die "未知参数: $1（--only <module_id> / --push）" ;;
  esac
done

[[ -d "$MODELS_ROOT" ]] || die "EntryPoint_Models 目录不存在: $MODELS_ROOT（可用 EP_MODELS_ROOT 指定）"

verb() { sed -n 's/^version = "\(.*\)"/\1/p' "$1" | head -1; }

# 写 ADAPTER.md（反向同步总览）；多适配器 repo 用 README.md 承担该角色
write_adapter_readme() {
  local repo="$1" mn="$2" up="$3" out="$4" header="$5" ghrepo="$6"
  cat > "$out" <<EOF
# $header

本仓库是 [EntryPoint](https://github.com/PegionFish/EntryPoint) 对上游
[$mn]($up) 的适配层镜像，反向同步自主仓库。

- 同步自: https://github.com/PegionFish/EntryPoint（source commit: \`$EP_HEAD\`，$SINCE）
- GitHub 插件 repo: https://github.com/PegionFish/$ghrepo
- 上游: $up
- 主仓库对应目录: modules/$repo
- 同步工具: scripts/sync-model-repos.sh
- 用法文档见仓库根 README.md；模块接口见主仓库 docs/MODULE_SPEC.md

## 内容

| 文件 | 说明 |
|------|------|
| \`adapter.py\` | HTTP 推理适配器（FastAPI 服务） |
| \`module.toml\` | EntryPoint 模块清单（模型注册、参数 schema、后端要求） |
| \`requirements*.txt\` | 依赖（默认/cuda/rocm/openvino 按后端分流） |
| \`upstream.json\` | 同步血缘元数据 |
| \`README.md\` | 模块用法文档 |

## 同步

\`\`\`bash
# 从主仓库刷新
> /home/bob/EntryPoint/scripts/sync-model-repos.sh --only $repo

# 推送 GitHub
git push -u origin main
\`\`\`
EOF
}

patch_upstream_json() { # $1=json $2=upstream $3=module_id $4=module_name $5=module_dir
  local json="$1" up="$2" mid="$3" mn="$4" mdl="$5" ver
  ver="$(verb "$mdl/module.toml")"
  local models
  models="$(python3 - "$json" <<'PY'
import json,sys
try:
    d=json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
def walk(o):
    if isinstance(o,dict):
        for k,v in o.items():
            if k=='models' and isinstance(v,list):
                for x in v:
                    if isinstance(x,str): print(x)
            else: walk(v)
    elif isinstance(o,list):
        for x in o: walk(x)
walk(d)
PY
)"
  python3 - "$json" "$up" "$mid" "$mn" "$ver" "$EP_HEAD" "$SINCE" "$models" <<'PY'
import json,os,sys
json_path,up,mid,mn,ver,head,since=sys.argv[1:8]
models=[x for x in sys.argv[8].split('\n') if x.strip()]
d={}
if os.path.exists(json_path):
    d=json.load(open(json_path))
ids=d.get('module_ids',[])
ids=[x for x in ids if isinstance(x,str)]
if mid not in ids:
    ids=ids+[mid]
d['upstream']=up
d['module_ids']=sorted(ids)
d['module_name']=mn
d['module_version']=ver
d['adapted_from']='https://github.com/PegionFish/EntryPoint'
d['entrypoint_commit']=head
d['synced_at']=since
d['models']=sorted(set(models))
json.dump(d,open(json_path,'w'),indent=2,ensure_ascii=False)
print(f"  upstream.json OK: {mid} v{ver} @ {head}")
PY
}

sync_module() { # $1=module_id
  local mid="$1"
  [[ -d "$EP_ROOT/modules/$mid" ]] || die "modules/$mid 不存在"
  local repo up mn
  IFS='|' read -r repo up mn <<< "${MOD_TO_REPO[$mid]}"
  local target="$MODELS_ROOT/$repo"
  [[ -d "$target" && -d "$target/.git" ]] || die "目标 repo 未初始化: $target"
  local dest="$target"
  local multi=0
  if [[ -d "$target/adapters" ]]; then multi=1; dest="$target/adapters/$mid"; fi

  log "$(printf '%-18s -> %s%s' "$mid" "$target" "$([[ $multi -eq 1 ]] && echo "/adapters/$mid")")"
  rsync -a --delete --exclude='.git' --exclude='__pycache__' --exclude='README.md' \
        --exclude='ADAPTER.md' --exclude='upstream.json' \
        "$EP_ROOT/modules/$mid/" "$dest/"

  # README / ADAPTER.md / upstream.json 的独立维护
  if [[ $multi -eq 1 ]]; then
    cp "$EP_ROOT/modules/$mid/README.md" "$dest/README.md"
  else
    cp "$EP_ROOT/modules/$mid/README.md" "$target/README.md"
    write_adapter_readme "$mid" "$mn" "$up" "$target/ADAPTER.md" "$mn" "$repo"
  fi
  patch_upstream_json "$target/upstream.json" "$up" "$mid" "$mn" "$EP_ROOT/modules/$mid"

  if [[ -n "$(cd "$target" && git status --porcelain)" ]]; then
    ( cd "$target"
      git add -A
      git -c user.email=entrypoint@local -c user.name="EntryPoint Adapter Sync" \
          commit -q -m "sync(adapter): $mid from EntryPoint main@$EP_HEAD ($SINCE)"
      log "  已提交 $(git log --oneline -1)" )
  else
    log "  无变更，跳过提交"
  fi
  if [[ $PUSH -eq 1 ]]; then
    ( cd "$target" && git push origin HEAD 2>/dev/null && log "  已推送 origin" \
      || log "  推送失败或无 origin（先 git remote add origin ...）")
  fi
}

if [[ -n "$ONLY" ]]; then
  sync_module "$ONLY"
else
  for mid in "${!MOD_TO_REPO[@]}"; do sync_module "$mid"; done
fi
log "完成 ✅  主仓库 HEAD=$EP_HEAD"
