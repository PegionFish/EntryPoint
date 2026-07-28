# EntryPoint

EntryPoint是一款AI创意/内容处理工具管控平台，能够统一调度多种本地 AI 模型服务，支持多 GPU 分配与 DAG 管线工作流。

## 特性

- 🖥️ 原生桌面 GUI（egui + Rust）
- 🌐 WebUI 管理界面（React + TypeScript）— 浏览器远程管控
- 🎮 多 GPU 调度 — 不同模型跑在不同显卡上
- 🔧 服务生命周期管理 — 启动/停止/监控/日志
- 🔗 DAG 管线引擎 — 视频→降噪→ASR→翻译→SRT 一键完成
- 📦 模型管理 — 独立配置/下载/更新
- 🐧 Linux 服务器部署 — systemd 服务 + 防火墙配置

## 文档

- [设计文档](DESIGN.md)
- [部署指南](docs/DEPLOYMENT.md)
- [WebUI 设计系统](docs/DESIGN_SYSTEM.md)
- [模块接入规范](docs/MODULE_SPEC.md)
- [配置参考](docs/CONFIG_REFERENCE.md)

## 开发环境搭建

### 前置依赖

| 工具 | 版本要求 | 安装方式 |
|---|---|---|
| Rust | stable (1.97+) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node.js | 20+ | `dnf module install nodejs:20` 或 [nvm](https://github.com/nvm-sh/nvm) |
| uv | 最新 | `curl -LsSf https://astral.sh/uv/install.sh \| sh` |
| ffmpeg | 5.x+ | `dnf install ffmpeg` 或静态构建 |

### 克隆与初始化

```bash
git clone <repo-url> EntryPoint
cd EntryPoint
```

## WebUI 开发指南

WebUI 前端位于 `crates/ep-webui/frontend/`，使用 Vite 开发服务器：

```bash
cd crates/ep-webui/frontend
npm install
npm run dev
```

- 开发服务器运行在 `http://localhost:5173`
- API 请求自动代理到后端 `http://localhost:9800`（含 WebSocket）
- 后端需同时运行：`cargo run -p ep-daemon`

技术栈：React 19 + TypeScript + Vite + TailwindCSS 4 + shadcn/ui + React Flow + Zustand

## 构建

使用统一构建脚本：

```bash
./scripts/build.sh
```

该脚本执行：
1. `cargo build --release` — 编译 Rust 后端（ep-daemon 二进制）
2. `npm ci && npm run build` — 构建前端静态资源到 `crates/ep-webui/static/`

构建产物：
- 后端二进制：`target/release/ep-daemon`
- 前端静态文件：`crates/ep-webui/static/`

## Linux 部署指南

### 1. 构建

```bash
./scripts/build.sh
```

### 2. 安装 systemd 服务

```bash
./scripts/install-service.sh
```

该脚本将 `entrypoint.service` 复制到 `/etc/systemd/system/` 并启用开机自启。

启动服务：

```bash
sudo systemctl start entrypoint
```

### 3. 防火墙配置

开放 WebUI 端口（默认 9800）：

```bash
sudo firewall-cmd --permanent --add-port=9800/tcp
sudo firewall-cmd --reload
```

### 4. 访问

浏览器打开 `http://<服务器IP>:9800` 即可使用 WebUI。

### 5. 日志查看

```bash
# 实时跟踪日志
journalctl -u entrypoint -f

# 查看最近 100 行
journalctl -u entrypoint -n 100
```

> ⚠️ 安全提示：默认配置仅允许局域网访问（IP 过滤中间件）。如需公网访问，在 `config/app.toml` 中设置 `[server] allow_public = true`。

详细部署文档见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)。

## 状态

✅ 核心功能已完成，WebUI 已实现
