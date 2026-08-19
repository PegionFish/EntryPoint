#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT_DIR="$(pwd)"
export RUST_LOG="${RUST_LOG:-ep_daemon=info,ep_core=info}"

# 共享 CUDA 库目录（PACK_UNIFY_PLAN §3.1）：
# runtime/cuda-libs 存在时前置注入 LD_LIBRARY_PATH（保留继承值）；
# 目录为可选资产（.gitignore 忽略 runtime/），缺失时回退原有系统 CUDA 路径默认值。
# 模块子进程的注入另由 daemon 代码负责（ep-core process.rs），此处覆盖 daemon 自身。
CUDA_LIBS_DIR="${EP_CUDA_LIBS_DIR:-$ROOT_DIR/runtime/cuda-libs}"
if [ -d "$CUDA_LIBS_DIR" ]; then
    export LD_LIBRARY_PATH="$CUDA_LIBS_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
else
    export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:-/usr/local/cuda/lib64}"
fi
exec ./target/release/ep-daemon
