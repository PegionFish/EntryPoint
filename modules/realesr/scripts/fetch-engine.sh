#!/usr/bin/env bash
# fetch-engine.sh — 从上游官方 release 获取 realesrgan-ncnn-vulkan 引擎（vulkan 兜底路线）
# 用法：bash scripts/fetch-engine.sh [windows|linux]
# 产物：bin/<os>-<arch>/realesrgan-ncnn-vulkan(.exe)
# 上游：xinntao/Real-ESRGAN release v0.2.5.0（MIT/BSD-3 公版；禁用 W2xEX 重编译版）
set -euo pipefail

OS_KEY="${1:-}"
REPO_URL="https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0"
ZIP_LINUX="realesrgan-ncnn-vulkan-20220424-ubuntu.zip"
ZIP_WINDOWS="realesrgan-ncnn-vulkan-20220424-windows.zip"
MODULE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$OS_KEY" in
  windows) ZIP_NAME="$ZIP_WINDOWS"; DEST="$MODULE_DIR/bin/windows-x86_64"; BIN_NAME="realesrgan-ncnn-vulkan.exe" ;;
  linux|"") ZIP_NAME="$ZIP_LINUX"; DEST="$MODULE_DIR/bin/linux-x86_64"; BIN_NAME="realesrgan-ncnn-vulkan" ;;
  *) echo "usage: $0 [windows|linux]" >&2; exit 2 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo ">> downloading $REPO_URL/$ZIP_NAME"
curl -fL --retry 3 -o "$TMP/$ZIP_NAME" "$REPO_URL/$ZIP_NAME"
mkdir -p "$TMP/unpack" "$DEST"
unzip -q "$TMP/$ZIP_NAME" -d "$TMP/unpack"

SRC_BIN="$(find "$TMP/unpack" -type f -name "$BIN_NAME" | head -n1)"
if [ -z "$SRC_BIN" ]; then
  echo "engine binary '$BIN_NAME' not found inside zip" >&2
  exit 1
fi
cp "$SRC_BIN" "$DEST/"
chmod +x "$DEST/$BIN_NAME"

echo ">> engine placed: $DEST/$BIN_NAME"
echo ">> ncnn 权重（param+bin）请经平台模型管理器下载 realesrgan-animevideov3-x4-ncnn"
echo "   变体后手动解压（zip 不在 URL 自动解压范围），解压出的 models/ 目录保持原样。"
