#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
export RUST_LOG="${RUST_LOG:-ep_daemon=info,ep_core=info}"
export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:-/usr/local/cuda/lib64}"
exec ./target/release/ep-daemon
