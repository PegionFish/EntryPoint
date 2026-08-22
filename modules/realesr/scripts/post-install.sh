#!/usr/bin/env bash
# post-install.sh — realesr 的 ep-core post-install 钩子（basicsr 依赖链修复）
#
# 由 EnvManager::run_post_install_hook 在依赖安装完成、`.ep_deps_hash` 落盘前
# 经 bash 调用（MODULE_SPEC §2.6 v1.3-draft），注入环境变量：
#   VIRTUAL_ENV=<venv 目录>、EP_BACKEND=<backend 小写名>
#
# 职责（basicsr 1.4.2 依赖链两步安装法）：
#   basicsr==1.4.2 的 setup.py 硬声明 install_requires tb-nightly——该包已从
#   PyPI 移除，常规解析必然失败；而运行时真正需要的只有 RRDBNet/SRVGG 架构
#   定义与 RealESRGANer 推理器。故 requirements-torch.txt 不含这两包，
#   由本钩子以 --no-deps 安装并补齐其最小运行时依赖（实证集见探针记录：
#   scipy/lmdb/tqdm/addict/future/pyyaml/requests）。
#   torchvision>=0.17 移除 functional_tensor 模块由 adapter 内 shim 兜底，
#   与本钩子无关。
#
# 契约：
#   - 幂等可重入：--no-deps 定版重装天然幂等；
#   - 钩子失败时哈希不落盘，下次进入必然重装并重跑本脚本。
set -euo pipefail

log() { printf '[post-install] %s\n' "$*"; }
die() { printf '[post-install][FATAL] %s\n' "$*" >&2; exit 2; }

VENV_PY="${VIRTUAL_ENV:-}/bin/python"
[[ -x "$VENV_PY" ]] || die "VIRTUAL_ENV 未注入或 python 缺失: ${VIRTUAL_ENV:-<empty>}"

UV_BIN="${UV_BIN:-uv}"

log "--no-deps 安装 basicsr==1.4.2 / realesrgan==0.3.0 ..."
"$UV_BIN" pip install -p "$VENV_PY" --no-deps \
    "basicsr==1.4.2" "realesrgan==0.3.0" \
    || die "basicsr/realesrgan 安装失败"

log "补齐最小运行时依赖 ..."
"$UV_BIN" pip install -p "$VENV_PY" \
    "scipy>=1.10" "lmdb>=1.4" "tqdm>=4.66" \
    "addict" "future" "pyyaml" "requests" \
    || die "运行时依赖补齐失败"

log "导入自检 ..."
"$VENV_PY" - <<'PYEOF'
import sys, types
import torchvision.transforms.functional as _F
_shim = types.ModuleType("torchvision.transforms.functional_tensor")
_shim.rgb_to_grayscale = _F.rgb_to_grayscale
sys.modules.setdefault("torchvision.transforms.functional_tensor", _shim)
from realesrgan import RealESRGANer  # noqa: F401
from basicsr.archs.rrdbnet_arch import RRDBNet  # noqa: F401
from realesrgan.archs.srvgg_arch import SRVGGNetCompact  # noqa: F401
print("[post-install] import self-check OK")
PYEOF

log "done"
