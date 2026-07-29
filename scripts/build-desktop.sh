#!/usr/bin/env bash
# 桌面端 release 构建脚本
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "=== EntryPoint Desktop — Release Build ==="

# 1. Rust 构建
echo "[1/3] cargo build --release -p ep-desktop ..."
cargo build --release -p ep-desktop

# 2. 验证二进制
BINARY="target/release/entrypoint"
if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: 二进制文件未生成: $BINARY"
    exit 1
fi
echo "[2/3] 二进制已生成: $BINARY ($(du -h "$BINARY" | cut -f1))"

# 3. 可选：构建 WebUI 前端
if [[ -d "crates/ep-webui/frontend" ]]; then
    echo "[3/3] 构建 WebUI 前端 ..."
    cd crates/ep-webui/frontend
    if command -v npm &>/dev/null; then
        npm ci --silent 2>/dev/null || npm install --silent
        npm run build
        echo "WebUI 前端构建完成: dist/"
    else
        echo "SKIP: npm 未安装，跳过 WebUI 前端构建"
    fi
else
    echo "[3/3] SKIP: 未找到 WebUI 前端目录"
fi

echo ""
echo "=== 构建完成 ==="
echo "桌面端: $BINARY"
echo "运行:   $BINARY"