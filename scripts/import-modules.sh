#!/usr/bin/env bash
# ============================================================================
# import-modules.sh — 从插件 repo 外部导入模型适配层到 modules/
#
# 背景（2026-09-09 起）：主仓主线只携带调度引擎与内置节点，modules/ 下适配层
# 不入 git（.gitignore: modules/*）。fresh clone / 新部署机由此脚本从插件
# repo 导入适配层（daemon 只扫描 modules/，导入后即可发现模块）。
#
# 用法:
#   ./scripts/import-modules.sh                # 导入映射表中全部缺失模块
#   ./scripts/import-modules.sh waifu2x-unlimited rife   # 只导入指定模块
#
# 来源优先级（单仓多适配器 repo 自动落到 adapters/<module_id>）：
#   1. 本地插件 repo：${EP_MODELS_ROOT:-/home/bob/EntryPoint_Models}/<repo>
#   2. GitHub 浅克隆：https://github.com/PegionFish/<repo>.git（缓存到
#      ${EP_MODELS_ROOT} 下，与本地 repo 布局一致）
#
# 依赖：git、rsync、bash。映射表单一事实源 = sync-model-repos.sh 的
# MOD_TO_REPO（本脚本运行时提取，勿在两处维护）。
# ============================================================================
set -euo pipefail

EP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SYNC_SCRIPT="$EP_ROOT/scripts/sync-model-repos.sh"
MODELS_ROOT="${EP_MODELS_ROOT:-/home/bob/EntryPoint_Models}"
ORIGIN_PREFIX="${EP_PLUGIN_ORIGIN:-https://github.com/PegionFish}"

log() { printf '[import] %s\n' "$*"; }
die() { printf '[import][失败] %s\n' "$*" >&2; exit 1; }

[[ -f "$SYNC_SCRIPT" ]] || die "找不到 $SYNC_SCRIPT"

# 提取 MOD_TO_REPO 声明块并求值（与 sync 脚本共享同一映射）
MAP_BLOCK="$(sed -n '/^declare -A MOD_TO_REPO=/,/^)/p' "$SYNC_SCRIPT")"
[[ -n "$MAP_BLOCK" ]] || die "sync 脚本中未找到 MOD_TO_REPO 映射"
eval "$MAP_BLOCK"

# rsync 规则与 sync-module 对偶：仓库级元数据不落 modules/，__pycache__/.git 不导入
import_one() { # $1=module_id
  local mid="$1"
  [[ -d "$EP_ROOT/modules/$mid" ]] && { log "$(printf '%-18s 已存在，跳过' "$mid")"; return 0; }
  local row repo
  row="${MOD_TO_REPO[$mid]:-}"
  [[ -n "$row" ]] || die "modules/$mid 不在映射表中（先在 sync-model-repos.sh 登记）"
  IFS='|' read -r repo _up _mn <<< "$row"

  local src="$MODELS_ROOT/$repo"
  if [[ ! -d "$src/.git" ]]; then
    log "$repo 本地不存在 → 从 $ORIGIN_PREFIX/$repo.git 浅克隆"
    git clone --depth 1 "$ORIGIN_PREFIX/$repo.git" "$src"
  fi
  local adapter_dir="$src"
  [[ -d "$src/adapters/$mid" ]] && adapter_dir="$src/adapters/$mid"
  [[ -f "$adapter_dir/module.toml" ]] || die "$repo 内未找到 $mid 的适配层（module.toml 缺失）"

  mkdir -p "$EP_ROOT/modules/$mid"
  rsync -a --exclude='.git' --exclude='__pycache__' \
        --exclude='README.md' --exclude='ADAPTER.md' --exclude='upstream.json' \
        "$adapter_dir/" "$EP_ROOT/modules/$mid/"
  # 用法文档随 repo 根的 README 一并落位（与 modules/<id>/README.md 历史形态一致）
  [[ -f "$adapter_dir/README.md" ]] && cp "$adapter_dir/README.md" "$EP_ROOT/modules/$mid/README.md"
  log "$(printf '%-18s <- %s' "$mid" "${adapter_dir#$MODELS_ROOT/}")"
}

TARGETS=("$@")
if [[ ${#TARGETS[@]} -eq 0 ]]; then
  TARGETS=("${!MOD_TO_REPO[@]}")
  log "导入映射表全部缺失模块（$(printf '%s ' "${TARGETS[@]}")）"
fi
for mid in "${TARGETS[@]}"; do
  import_one "$mid"
done

log "完成 ✅  daemon 重启/重扫后即可在 modules/ 发现新导入的模块"
