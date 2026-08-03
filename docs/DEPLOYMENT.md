# EntryPoint 部署指南

本文档描述如何在 Linux 服务器上构建、部署和运行 EntryPoint。

---

## 1. 前置要求

| 项目 | 要求 | 说明 |
|---|---|---|
| 操作系统 | RHEL 9 / Rocky Linux 9 / 同类 | 其他 Linux 发行版亦可（需调整包管理命令） |
| CUDA | 13.0+ | NVIDIA GPU 加速（可选，无 GPU 时回退 CPU） |
| Rust | stable (1.97+) | 通过 rustup 安装 |
| Node.js | 20+ | 前端构建 |
| uv | 最新 | Python 虚拟环境管理 |
| ffmpeg | 5.x+ | 音视频处理 |

### 安装依赖

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Node.js 20 (RHEL/Rocky)
sudo dnf module install nodejs:20

# uv
curl -LsSf https://astral.sh/uv/install.sh | sh

# ffmpeg
sudo dnf install ffmpeg
# 或使用静态构建：https://johnvansickle.com/ffmpeg/
```

---

## 2. 构建

使用统一构建脚本（服务器包）：

```bash
cd /server/EntryPoint
./build.sh server
```

脚本依次执行：

1. **Rust 后端**：`cargo build --release -p ep-daemon`
   - 产物：`target/release/ep-daemon`
2. **打包**：tar.gz 兜底包（含 systemd 服务 + install.sh）+ 自动探测 deb/rpm/PKGBUILD

构建完成后，解压 tar.gz 运行 `install.sh` 即可部署到 `/opt/entrypoint`。

---

## 3. 配置

主配置文件：`config/app.toml`

### 服务器配置（[server] 段）

```toml
[server]
host = "0.0.0.0"       # 监听地址
port = 9800            # 监听端口
allow_public = false   # 是否允许公网访问（默认仅局域网）
```

**安全说明**：
- `allow_public = false`（默认）：启用 IP 过滤中间件，仅允许 RFC 1918 私有地址（10.x / 172.16-31.x / 192.168.x）访问
- `allow_public = true`：关闭 IP 过滤，允许任何来源访问。**仅在配置了反向代理 + HTTPS 时使用**

### 其他常用配置

```toml
[compute]
strategy = "least_memory"     # 设备分配策略：manual | least_memory | round_robin | single
refresh_interval_secs = 5     # 设备状态刷新间隔

[models]
cache_dir = "./models"        # 模型缓存目录
hf_endpoint = "https://huggingface.co"  # HuggingFace 端点（可换镜像）

[ports]
range_start = 18000           # 模块端口分配范围
range_end = 19000
```

完整配置参考见 [CONFIG_REFERENCE.md](CONFIG_REFERENCE.md)。

---

## 4. 安装 systemd 服务

```bash
./scripts/install-service.sh
```

该脚本执行：
1. 复制 `scripts/entrypoint.service` 到 `/etc/systemd/system/`
2. `systemctl daemon-reload`
3. `systemctl enable entrypoint`（开机自启）

### 服务管理

```bash
# 启动
sudo systemctl start entrypoint

# 停止
sudo systemctl stop entrypoint

# 重启
sudo systemctl restart entrypoint

# 查看状态
sudo systemctl status entrypoint

# 开机自启状态
systemctl is-enabled entrypoint
```

### 服务文件说明

`scripts/entrypoint.service` 关键配置：

```ini
[Service]
Type=simple
User=bob                              # 运行用户（按需修改）
WorkingDirectory=/server/EntryPoint   # 工作目录（按需修改）
ExecStart=/server/EntryPoint/target/release/ep-daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=ep_daemon=info,ep_core=info
Environment=LD_LIBRARY_PATH=/usr/local/cuda/lib64
```

> ⚠️ 部署到其他机器时，需修改 `User`、`WorkingDirectory`、`ExecStart` 路径。

---

## 5. 防火墙与 SELinux 配置

> 💡 运行 `scripts/install-service.sh` 会**自动**完成下述防火墙与 SELinux 配置（幂等，可重复执行）。以下内容用于手动操作或排障。

### 5.1 防火墙（firewalld）

开放 WebUI 端口（默认 9800）：

```bash
# 永久开放端口
sudo firewall-cmd --permanent --add-port=9800/tcp
sudo firewall-cmd --reload

# 验证
sudo firewall-cmd --list-ports
```

如修改了 `config/app.toml` 中的端口，需对应调整防火墙规则。

### 5.2 SELinux

RHEL 默认 SELinux 为 `Enforcing`。daemon 绑定 9800 端口前，需为该端口添加 SELinux 标签，否则在 systemd 受限域下启动可能被拒绝绑定。

```bash
# 查看当前 SELinux 状态
getenforce

# 为 9800/tcp 添加 http_port_t 标签（幂等：已存在则用 -m 修改）
sudo semanage port -a -t http_port_t -p tcp 9800
# 若提示已存在不同标签，改用：
# sudo semanage port -m -t http_port_t -p tcp 9800

# 验证
sudo semanage port -l | grep 9800
```

若缺少 `semanage` 命令，先安装：

```bash
sudo dnf install -y policycoreutils-python-utils
```

**关于服务域**：`entrypoint.service` 未指定 `SELinuxContext=`，systemd 默认以 `unconfined_service_t` 运行——这对需要派生 Python 模块、ffmpeg 等子进程的应用是合适的（完全受限策略会阻断子进程派生）。配合上面的端口标签即可正常工作。

**排障**：若 systemd 启动后仍被 SELinux 拦截，查看拒绝日志并生成策略：

```bash
sudo ausearch -m avc -ts recent          # 查看最近的 SELinux 拒绝
sudo journalctl -u entrypoint -n 50      # 查看服务日志
```

---

## 6. 访问 WebUI

浏览器打开：

```
http://<服务器IP>:9800
```

首次访问即可看到仪表盘，包含设备状态、模块列表等。

---

## 7. 日志查看

EntryPoint 通过 systemd journal 输出日志：

```bash
# 实时跟踪日志
journalctl -u entrypoint -f

# 查看最近 100 行
journalctl -u entrypoint -n 100

# 查看今天的日志
journalctl -u entrypoint --since today

# 按时间范围查看
journalctl -u entrypoint --since "2026-07-29 10:00" --until "2026-07-29 12:00"
```

### 日志级别

通过 `RUST_LOG` 环境变量控制（在 service 文件中配置）：

| 级别 | 说明 |
|---|---|
| `error` | 仅错误 |
| `warn` | 警告 + 错误 |
| `info` | 常规信息（默认） |
| `debug` | 调试信息 |
| `trace` | 详细追踪 |

示例：临时提高日志级别运行：

```bash
sudo systemctl stop entrypoint
sudo RUST_LOG=debug /server/EntryPoint/target/release/ep-daemon
```

---

## 8. 故障排除

### 服务启动失败

```bash
# 查看详细错误
journalctl -u entrypoint -n 50 --no-pager

# 手动运行测试
cd /server/EntryPoint
./target/release/ep-daemon
```

常见原因：
- **端口被占用**：`ss -tlnp | grep 9800`，修改配置或停止占用进程
- **权限不足**：确认运行用户对工作目录有读写权限
- **CUDA 库缺失**：确认 `LD_LIBRARY_PATH` 包含 CUDA 库路径

### GPU 未检测到

```bash
# 检查 NVIDIA 驱动
nvidia-smi

# 确认 CUDA 库路径
ls /usr/local/cuda/lib64/libcudart.so*
```

- 无 GPU 时系统自动回退 CPU 模式，不影响基本功能
- 确认 service 文件中 `Environment=LD_LIBRARY_PATH=/usr/local/cuda/lib64` 路径正确

### WebUI 无法访问

1. 确认服务运行中：`systemctl status entrypoint`
2. 确认端口监听：`ss -tlnp | grep 9800`
3. 确认防火墙：`sudo firewall-cmd --list-ports`
4. 确认 IP 过滤：默认仅允许局域网 IP，公网访问需设置 `allow_public = true`
5. 浏览器直接访问 `http://localhost:9800`（服务器本机测试）

### 模块启动失败

```bash
# 检查 uv 是否可用
which uv && uv --version

# 检查 Python
python3 --version

# 手动测试模块环境
cd /server/EntryPoint
uv venv runtime/venvs/<module-id>/
```

### 构建失败

- **Rust 编译错误**：确认 `rustup update` 到最新 stable
- **npm 错误**：删除 `node_modules` 和 `package-lock.json` 后重新 `npm install`
- **磁盘空间**：release 构建需要约 2GB 临时空间

---

## 9. 更新部署

```bash
cd /server/EntryPoint

# 拉取最新代码
git pull

# 重新构建服务器包
./build.sh server

# 解压 tar.gz 后运行 install.sh 完成安装（或直接用源码树安装）
sudo systemctl restart entrypoint

# 确认启动成功
journalctl -u entrypoint -f
```

---

## 10. 安全建议

1. **默认局域网模式**：`allow_public = false` 时仅允许私有 IP 访问，适合内网部署
2. **公网部署**：建议前置 Nginx/Caddy 反向代理 + HTTPS，再设置 `allow_public = true`
3. **最小权限**：service 文件使用非 root 用户运行
4. **防火墙**：仅开放必要端口（9800 + 模块端口范围 18000-19000 按需）
5. **模型目录权限**：模型缓存目录设置为运行用户所有
