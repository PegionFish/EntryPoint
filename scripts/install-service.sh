#!/usr/bin/env bash
#
# 安装 EntryPoint systemd 服务，并配置防火墙与 SELinux。
# 需要 root 权限（脚本内部使用 sudo）。
#
# 用法： bash scripts/install-service.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIG_FILE="$PROJECT_ROOT/config/app.toml"

# 从 config/app.toml 读取端口（[server] 段），默认 9800
PORT="$(grep -E '^[[:space:]]*port[[:space:]]*=' "$CONFIG_FILE" 2>/dev/null \
  | head -1 | grep -oE '[0-9]+' | head -1 || true)"
PORT="${PORT:-9800}"

echo "==> 项目根目录: $PROJECT_ROOT"
echo "==> WebUI 端口:  $PORT"

# ── 1. 安装 systemd 服务 ─────────────────────────────────────────────────────
echo "==> 安装 systemd 服务..."
# unit（scripts/entrypoint.service）面向 /opt/entrypoint 部署布局（bin/ep-daemon）。
# 先校验目标二进制存在，避免安装出必然启动失败的 unit；若本机是源码检出方式
# （二进制在 target/release/），请改用 ./build.sh server 产物内 install.sh
# （自动安装到 /opt/entrypoint 并注册服务）。
if [[ ! -x /opt/entrypoint/bin/ep-daemon ]]; then
  echo "!!  未找到 /opt/entrypoint/bin/ep-daemon —— 本 unit 面向 /opt/entrypoint 布局。"
  echo "!!  请先执行 ./build.sh server 并在产物目录运行 install.sh（安装到 /opt/entrypoint），"
  echo "!!  或按实际部署路径修改 scripts/entrypoint.service 的 ExecStart/WorkingDirectory 后重试。"
  exit 1
fi
sudo cp "$SCRIPT_DIR/entrypoint.service" /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable entrypoint

# ── 2. 防火墙配置（firewalld）────────────────────────────────────────────────
if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
  echo "==> 配置防火墙：开放 ${PORT}/tcp..."
  if sudo firewall-cmd --permanent --add-port="${PORT}/tcp" 2>/dev/null; then
    sudo firewall-cmd --reload
    echo "    防火墙已开放 ${PORT}/tcp"
  else
    # 端口已开放时 firewall-cmd 返回非零（ALREADY_ENABLED），视为成功
    echo "    端口 ${PORT}/tcp 已开放（或配置未变更）"
  fi
  echo "    当前开放端口: $(sudo firewall-cmd --list-ports)"
else
  echo "!!  未检测到运行中的 firewalld，跳过防火墙配置"
fi

# ── 3. SELinux 配置 ──────────────────────────────────────────────────────────
if command -v getenforce >/dev/null 2>&1 && [ "$(getenforce 2>/dev/null)" != "Disabled" ]; then
  echo "==> 配置 SELinux：为 ${PORT}/tcp 添加 http_port_t 标签..."
  if command -v semanage >/dev/null 2>&1; then
    # 幂等：先尝试新增（-a），若端口已有其它标签则改为修改（-m）
    if sudo semanage port -a -t http_port_t -p tcp "$PORT" 2>/dev/null; then
      echo "    已添加端口标签 http_port_t: ${PORT}/tcp"
    elif sudo semanage port -m -t http_port_t -p tcp "$PORT" 2>/dev/null; then
      echo "    已更新端口标签为 http_port_t: ${PORT}/tcp"
    else
      echo "!!  SELinux 端口标签配置失败，请手动检查：semanage port -l | grep $PORT"
    fi
  else
    echo "!!  未找到 semanage，请安装 policycoreutils-python-utils 后手动添加端口标签："
    echo "      sudo semanage port -a -t http_port_t -p tcp $PORT"
  fi
else
  echo "==> SELinux 已禁用或不可用，跳过"
fi

echo ""
echo "==> 安装完成。"
echo "    启动服务:  sudo systemctl start entrypoint"
echo "    查看状态:  systemctl status entrypoint"
echo "    查看日志:  journalctl -u entrypoint -f"
