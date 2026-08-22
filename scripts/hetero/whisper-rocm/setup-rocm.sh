#!/usr/bin/env bash
# setup-rocm.sh — faster-whisper rocm 后端实验环境从零搭建（WS-B / E1 准备）
#
# 流程：
#   1) 在 /tmp/opencode 下自建干净 venv（绝不触碰 runtime/venvs）
#   2) 安装 modules/faster-whisper/requirements-rocm.txt（ctranslate2==4.8.1 PyPI 占位 pin）
#   3) 下载 CTranslate2 官方 Release 的 rocm-python-wheels-Linux.zip 并校验 sha256
#   4) 解出与当前 Python cp 标签匹配的 HIP 轮子，--force-reinstall 同版本覆盖占位安装
#   5) 打印 ctranslate2 版本与 CUDA(=HIP) 设备数、支持的算精度
#
# 环境变量覆盖：CT2_ROCM_VERSION（默认 4.8.1）、WS_B_TMP_ROOT（默认 /tmp/opencode/ws-b-faster-whisper-rocm）、
#              ROCM_PATH（默认 /opt/rocm）
# 退出码：0 全部成功；2 前置工具缺失；3 依赖装好但 HIP 运行时不可用（缺 libamdhip64 等）
# 调研依据：scripts/hetero/whisper-rocm/README.md
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MODULE_DIR="${ROOT_DIR}/modules/faster-whisper"
REQ_FILE="${MODULE_DIR}/requirements-rocm.txt"

CT2_ROCM_VERSION="${CT2_ROCM_VERSION:-4.8.1}"
ZIP_URL="https://github.com/OpenNMT/CTranslate2/releases/download/v${CT2_ROCM_VERSION}/rocm-python-wheels-Linux.zip"
WORK_ROOT="${WS_B_TMP_ROOT:-/tmp/opencode/ws-b-faster-whisper-rocm}"
VENV_DIR="${WORK_ROOT}/venv"
DOWNLOAD_DIR="${WORK_ROOT}/downloads"
WHEELS_DIR="${WORK_ROOT}/wheels"
ROCM_PATH="${ROCM_PATH:-/opt/rocm}"
ZIP_PATH="${DOWNLOAD_DIR}/rocm-python-wheels-Linux.zip"

declare -A KNOWN_ZIP_SHA256=(
  ["4.8.1"]="2b454399aace4c76fe373e912f8d6a0d2033d6aa58dbfd438840aceca7cc64db"
)

log() { printf '[setup-rocm] %s\n' "$*"; }
die() { printf '[setup-rocm][FATAL] %s\n' "$*" >&2; exit 2; }

# ── 前置检查 ──────────────────────────────────────────────
command -v python3 >/dev/null 2>&1 || die "python3 not found"
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
  die "need curl or wget to fetch the release zip"
fi
[[ -f "${REQ_FILE}" ]] || die "requirements file missing: ${REQ_FILE}"

HIP_LIB_OK=0
if ldconfig -p 2>/dev/null | grep -q libamdhip64 \
   || compgen -G "${ROCM_PATH}/lib/libamdhip64.so*" >/dev/null; then
  HIP_LIB_OK=1
else
  log "WARN: ${ROCM_PATH}/lib lacks libamdhip64.so (HIP runtime) —"
  log "      wheel install can proceed, but 'import ctranslate2' will fail until host libs exist."
  log "      Remediation: install hip-runtime-amd / hip-libraries from AMD's apt repo, then re-run."
fi

mkdir -p "${WORK_ROOT}" "${DOWNLOAD_DIR}" "${WHEELS_DIR}"

# ── 1) 干净 venv ──────────────────────────────────────────
log "creating clean venv at ${VENV_DIR}"
python3 -m venv --clear "${VENV_DIR}"
"${VENV_DIR}/bin/python" -m pip install --upgrade pip --quiet

# ── 2) 基础依赖 + ctranslate2 占位 pin ────────────────────
log "installing ${REQ_FILE}"
"${VENV_DIR}/bin/python" -m pip install -r "${REQ_FILE}"

# ── 3) 下载并校验 Release zip ─────────────────────────────
EXPECTED_SHA="${KNOWN_ZIP_SHA256[${CT2_ROCM_VERSION}]:-}"
NEED_DL=1
if [[ -f "${ZIP_PATH}" ]]; then
  if [[ -n "${EXPECTED_SHA}" ]]; then
    ACTUAL_SHA="$(sha256sum "${ZIP_PATH}" | awk '{print $1}')"
    [[ "${ACTUAL_SHA}" == "${EXPECTED_SHA}" ]] && NEED_DL=0
  else
    NEED_DL=0  # 已有缓存且版本未登记 sha，直接复用
  fi
fi
if [[ "${NEED_DL}" -eq 1 ]]; then
  log "downloading ${ZIP_URL}"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 -o "${ZIP_PATH}" "${ZIP_URL}"
  else
    wget -q -O "${ZIP_PATH}" "${ZIP_URL}"
  fi
fi
if [[ -n "${EXPECTED_SHA}" ]]; then
  ACTUAL_SHA="$(sha256sum "${ZIP_PATH}" | awk '{print $1}')"
  if [[ "${ACTUAL_SHA}" != "${EXPECTED_SHA}" ]]; then
    die "sha256 mismatch for ${ZIP_PATH}: expected ${EXPECTED_SHA}, got ${ACTUAL_SHA}"
  fi
  log "sha256 verified: ${ACTUAL_SHA}"
else
  log "WARN: no pinned sha256 for CT2 v${CT2_ROCM_VERSION}; skipping integrity check"
fi

# ── 4) 解出匹配 cp 标签的 HIP 轮子并同版本覆盖 ─────────────
CP_TAG="$("${VENV_DIR}/bin/python" - <<'PY'
import sys
print(f"cp{sys.version_info.major}{sys.version_info.minor}")
PY
)"
log "extracting ${CP_TAG} manylinux x86_64 wheel from zip"
# 注意：zip 内轮子位于 temp-linux/ 等子目录下（参照 WhisperLive ROCm_whisper.md 的 unzip -j 用法），
# 故按 basename 匹配而非整路径。
rm -rf "${WHEELS_DIR:?}"/*
"${VENV_DIR}/bin/python" - "${ZIP_PATH}" "${WHEELS_DIR}" "${CP_TAG}" <<'PY'
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
WHEEL_FILE="$("${VENV_DIR}/bin/python" - "${WHEELS_DIR}" <<'PY'
import sys, pathlib
hits = sorted(pathlib.Path(sys.argv[1]).rglob("ctranslate2-*.whl"))
sys.exit(f"expected exactly one extracted wheel, found {len(hits)}") if len(hits) != 1 else print(hits[0])
PY
)"
log "overlay-installing HIP wheel (same version, force-reinstall): ${WHEEL_FILE}"
"${VENV_DIR}/bin/python" -m pip install --force-reinstall --no-deps "${WHEEL_FILE}"

# ── 5) 摘要：版本 + 设备 ─────────────────────────────────
export LD_LIBRARY_PATH="${ROCM_PATH}/lib:${ROCM_PATH}/lib/llvm/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
log "summary:"
set +e
"${VENV_DIR}/bin/python" - <<'PY'
import ctranslate2 as ct2
print("ctranslate2 version :", ct2.__version__)
n = ct2.get_cuda_device_count()
print("cuda(HIP) devices   :", n)
try:
    print("compute types(cuda) :", ct2.get_supported_compute_types("cuda"))
except Exception as exc:
    print("compute types(cuda) : unavailable ->", exc)
PY
RC=$?
set -e
if [[ ${RC} -ne 0 ]]; then
  printf '[setup-rocm][FAIL] ctranslate2 unusable (exit %s).\n' "${RC}" >&2
  printf '[setup-rocm] deps are installed; most likely the host HIP runtime is missing.\n' >&2
  printf '[setup-rocm] Check: ls %s/lib/libamdhip64.so*  |  apt install hip-runtime-amd\n' "${ROCM_PATH}" >&2
  exit 3
fi
log "done. venv: ${VENV_DIR}"
