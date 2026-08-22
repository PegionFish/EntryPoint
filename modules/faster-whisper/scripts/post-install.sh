#!/usr/bin/env bash
# post-install.sh — faster-whisper 的 ep-core post-install 钩子（真实用例）
#
# 由 EnvManager::run_post_install_hook 在依赖安装完成、`.ep_deps_hash` 落盘前
# 经 bash 调用（MODULE_SPEC §2.6 v1.3-draft），注入环境变量：
#   VIRTUAL_ENV=<venv 目录>、EP_BACKEND=<backend 小写名>（旧口径为空串）
#
# 职责（改造自 scripts/hetero/whisper-rocm/setup-rocm.sh 第 3/4 步）：
#   仅当 EP_BACKEND=rocm 且本地存在 CTranslate2 官方 Release 缓存 zip 时，
#   解出与当前 Python cp 标签匹配的 HIP 轮子，--force-reinstall 同版本覆盖
#   requirements-rocm.txt 安装的 PyPI 占位包（CTranslate2-ROCm 两步安装法）。
#
# 契约：
#   - EP_BACKEND 非 rocm / 缓存 zip 不存在 → 静默跳过（exit 0）；
#   - 幂等可重入：force-reinstall 天然幂等；临时目录用完即毁，无共享状态；
#     钩子失败时哈希不落盘，下次进入必然重装并重跑本脚本；
#   - HIP 运行时库缺失只告警不失败：轮子覆盖本身已完成，修复属宿主环境
#     操作（apt install hip-runtime-amd），无需重跑覆盖。
#
# 环境变量覆盖：
#   CT2_ROCM_VERSION   默认 4.8.1
#   CT2_ROCM_WHEEL_ZIP 缓存 zip 绝对路径（默认取 WS_B_TMP_ROOT 下载位）
#   WS_B_TMP_ROOT      默认 /tmp/opencode/ws-b-faster-whisper-rocm
set -euo pipefail

log() { printf '[post-install] %s\n' "$*"; }
warn() { printf '[post-install][WARN] %s\n' "$*" >&2; }
die() { printf '[post-install][FATAL] %s\n' "$*" >&2; exit 2; }

# ── 触发条件守卫 ──────────────────────────────────────────────
[[ "${EP_BACKEND:-}" == "rocm" ]] || { log "EP_BACKEND='${EP_BACKEND:-}' ≠ rocm, skip"; exit 0; }
[[ -n "${VIRTUAL_ENV:-}" ]] || die "VIRTUAL_ENV not injected"

VENV_PY="${VIRTUAL_ENV}/bin/python"
[[ -x "${VENV_PY}" ]] || die "venv interpreter missing: ${VENV_PY}"

CT2_ROCM_VERSION="${CT2_ROCM_VERSION:-4.8.1}"
WORK_ROOT="${WS_B_TMP_ROOT:-/tmp/opencode/ws-b-faster-whisper-rocm}"
ZIP_PATH="${CT2_ROCM_WHEEL_ZIP:-${WORK_ROOT}/downloads/rocm-python-wheels-Linux.zip}"

declare -A KNOWN_ZIP_SHA256=(
  ["4.8.1"]="2b454399aace4c76fe373e912f8d6a0d2033d6aa58dbfd438840aceca7cc64db"
)

# ── 缓存 zip 存在性：缺失属合法跳过（占位栈保留，由调用方决定是否补缓存重装）──
if [[ ! -f "${ZIP_PATH}" ]]; then
  warn "cached rocm wheels zip not found: ${ZIP_PATH} — keeping PyPI placeholder stack."
  warn "populate it first (see scripts/hetero/whisper-rocm/setup-rocm.sh), then reinstall deps."
  exit 0
fi

# ── sha256 校验 ───────────────────────────────────────────────
EXPECTED_SHA="${KNOWN_ZIP_SHA256[${CT2_ROCM_VERSION}]:-}"
if [[ -n "${EXPECTED_SHA}" ]]; then
  ACTUAL_SHA="$(sha256sum "${ZIP_PATH}" | awk '{print $1}')"
  [[ "${ACTUAL_SHA}" == "${EXPECTED_SHA}" ]] \
    || die "sha256 mismatch for ${ZIP_PATH}: expected ${EXPECTED_SHA}, got ${ACTUAL_SHA}"
  log "sha256 verified: ${ACTUAL_SHA}"
else
  warn "no pinned sha256 for CT2 v${CT2_ROCM_VERSION}; skipping integrity check"
fi

# ── 解出匹配 cp 标签的 manylinux x86_64 HIP 轮子 ───────────────
CP_TAG="$("${VENV_PY}" - <<'PY'
import sys
print(f"cp{sys.version_info.major}{sys.version_info.minor}")
PY
)"
WHEELS_DIR="$(mktemp -d)"
trap 'rm -rf "${WHEELS_DIR:?}"' EXIT
log "extracting ${CP_TAG} manylinux x86_64 wheel from ${ZIP_PATH}"
# 注意：zip 内轮子位于 temp-linux/ 等子目录下（参照 WhisperLive ROCm_whisper.md
# 的 unzip -j 用法），按 basename 匹配而非整路径。
"${VENV_PY}" - "${ZIP_PATH}" "${WHEELS_DIR}" "${CP_TAG}" <<'PY'
import re, sys, zipfile
from pathlib import PurePosixPath
zip_path, out_dir, cp_tag = sys.argv[1], sys.argv[2], sys.argv[3]
pat = re.compile(rf"^ctranslate2-[^/]*-{cp_tag}-{cp_tag}-manylinux.*x86_64\.whl$")
with zipfile.ZipFile(zip_path) as zf:
    hits = [n for n in zf.namelist() if pat.match(PurePosixPath(n).name)]
    if not hits:
        avail = "\n".join(PurePosixPath(n).name for n in zf.namelist() if n.endswith(".whl"))
        sys.exit(f"no matching wheel for {cp_tag}; available:\n{avail}")
    zf.extract(hits[0], out_dir)
PY
WHEEL_FILE="$("${VENV_PY}" - "${WHEELS_DIR}" <<'PY'
import sys, pathlib
hits = sorted(pathlib.Path(sys.argv[1]).rglob("ctranslate2-*.whl"))
sys.exit(f"expected exactly one extracted wheel, found {len(hits)}") if len(hits) != 1 else print(hits[0])
PY
)"

# ── 同版本覆盖安装（uv 优先，回退 venv 内 pip；均不触碰依赖解析）──
log "overlay-installing HIP wheel (same version, force-reinstall): ${WHEEL_FILE}"
if command -v uv >/dev/null 2>&1; then
  uv pip install --python "${VENV_PY}" --link-mode hardlink \
    --force-reinstall --no-deps "${WHEEL_FILE}"
elif "${VENV_PY}" -c "import pip" >/dev/null 2>&1; then
  "${VENV_PY}" -m pip install --force-reinstall --no-deps "${WHEEL_FILE}"
else
  die "neither uv nor venv pip available to overlay-install ${WHEEL_FILE}"
fi

# ── 摘要：HIP 可用性仅告警不失败（见头部契约说明）──────────────
ROCM_PATH="${ROCM_PATH:-/opt/rocm}"
export LD_LIBRARY_PATH="${ROCM_PATH}/lib:${ROCM_PATH}/lib/llvm/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
if ! "${VENV_PY}" - <<'PY'
import ctranslate2 as ct2
print("ctranslate2 version :", ct2.__version__)
print("cuda(HIP) devices   :", ct2.get_cuda_device_count())
try:
    print("compute types(cuda) :", ct2.get_supported_compute_types("cuda"))
except Exception as exc:
    print("compute types(cuda) : unavailable ->", exc)
PY
then
  warn "ctranslate2 unusable — most likely host HIP runtime is missing."
  warn "Remediation: ls ${ROCM_PATH}/lib/libamdhip64.so*  |  apt install hip-runtime-amd"
fi

log "done. venv: ${VIRTUAL_ENV}"
