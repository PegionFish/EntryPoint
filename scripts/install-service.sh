#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

sudo cp "$SCRIPT_DIR/entrypoint.service" /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable entrypoint
echo "Service installed. Start with: sudo systemctl start entrypoint"
