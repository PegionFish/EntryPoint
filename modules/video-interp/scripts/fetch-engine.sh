#!/usr/bin/env bash
# fetch-engine.sh — 从上游官方 release 获取 rife-ncnn-vulkan 引擎（vulkan 兜底路线）
# 用法：bash scripts/fetch-engine.sh [windows|linux]
# 产物：bin/<os>-<arch>/rife-ncnn-vulkan(.exe)
# 上游：nihui/rife-ncnn-vulkan release 20221029（MIT；禁用 W2xEX 重编译版）
# 注：整包 zip 同时含全部模型（models/<rife-*/>），上游无独立小体积模型资产。
set -euo pipefail

OS_KEY="${1:-}"
REPO_URL="https://github.com/nihui/rife-ncnn-vulkan/releases/download/20221029"
ZIP_LINUX="rife-ncnn-vulkan-20221029-ubuntu.zip"
ZIP_WINDOWS="rife-ncnn-vulkan-20221029-windows.zip"
MODULE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$OS_KEY" in
  windows) ZIP_NAME="$ZIP_WINDOWS"; DEST="$MODULE_DIR/bin/windows-x86_64"; BIN_NAME="rife-ncnn-vulkan.exe" ;;
  linux|"") ZIP_NAME="$ZIP_LINUX"; DEST="$MODULE_DIR/bin/linux-x86_64"; BIN_NAME="rife-ncnn-vulkan" ;;
  *) echo "usage: $0 [windows|linux]" >&2; exit 2 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo ">> downloading $REPO_URL/$ZIP_NAME (~411MB)"
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
echo ">> RIFE 权重（models/<rife-*/> param+bin）请经平台模型管理器下载 rife-v4.6-ncnn"
echo "   变体后手动解压（zip 不在 URL 自动解压范围），保持 models/ 子目录结构。"
