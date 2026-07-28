#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building Rust (release)..."
source "$HOME/.cargo/env"
cargo build --release

echo "==> Building WebUI frontend..."
cd crates/ep-webui/frontend
npm ci
npm run build
cd ../../..

echo "==> Build complete."
echo "    Binary:  target/release/ep-daemon"
echo "    WebUI:   crates/ep-webui/static/"
