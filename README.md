# EntryPoint

EntryPoint是一款AI创意/内容处理工具管控平台，能够统一调度多种本地 AI 模型服务，支持多 GPU 分配与 DAG 管线工作流。

## 特性

- 🖥️ 原生桌面 GUI（egui + Rust）— 模型管理、可视化管线编辑器、深色/浅色主题
- 🌐 WebUI 管理界面（React + TypeScript）— 浏览器远程管控
- 🎮 多 GPU 调度 — 不同模型跑在不同显卡上
- 🔧 服务生命周期管理 — 启动/停止/监控/日志捕获/健康检查
- 🔗 DAG 管线引擎 — 在线执行（React Flow 编辑器 + 服务端持久化 + 实时节点状态），视频→降噪→ASR→翻译→SRT 一键完成
- 🗂️ 任务中心 — 节点级状态跟踪 + 管线产物下载
- 📜 实时日志 — 搜索 / 过滤 / 导出
- 📦 模型管理 — 在线下载 / 浏览器上传 / 本地导入三种获取路径：
  - 在线下载：HuggingFace/ModelScope/URL 三源 + `[[models.mirrors]]` 镜像源，双源自选、WebSocket 实时进度、模型更新检查、代理支持（`[network]` 配置）
  - 浏览器上传：文件夹多文件 / zip / tar.gz，服务端流式落盘 + 解包 + 路径安全校验
  - 本地导入：服务器上已有目录导入，下载与导入均写入 `.ep_meta.json` 元数据
- 🐧 Linux 服务器部署 — systemd 服务 + 防火墙配置
- 📦 Arch Linux 打包 — PKGBUILD + .desktop 启动器

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

## 构建与打包

使用统一构建脚本（GUI 客户端与服务器分开打包）：

### Windows

```powershell
.\build.ps1 gui        # 桌面 GUI 客户端 zip（解压即用）
.\build.ps1 server     # 服务器 zip（ep-daemon + WebUI）
```

可选参数：`-Target debug|release`（默认 release）、`-SkipTest`、`-SkipClippy`、`-Clean`、`-OutputDir <dir>`（默认 dist）。

### Linux / macOS

```bash
./build.sh gui         # GUI 客户端包
./build.sh server      # 服务器包（仅 Linux；macOS 不支持）
```

- Linux GUI/server：tar.gz 兜底包 + 自动探测 deb（dpkg-deb）/ rpm（rpmbuild）/ Arch PKGBUILD（dist/arch-<mode>/）
- macOS：仅 GUI，产出 EntryPoint.app 并压缩为 zip
- 可选参数：`-t debug|release`、`--skip-test`、`--skip-clippy`、`--clean`、`-o <dir>`

构建产物：
- 二进制：`target/release/entrypoint`（GUI）、`target/release/ep-daemon`（服务器）
- 打包产物：`dist/` 下的 zip / tar.gz / deb / rpm

## Linux 部署指南

### 1. 构建

```bash
./build.sh server
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

## 桌面端构建

```bash
./scripts/build-desktop.sh
```

产物：`target/release/entrypoint`（egui 原生窗口应用）

功能：模型管理（下载/上传/导入三路径、双源自选下载、实时进度）、可视化管线编辑器（节点画布+贝塞尔连线+在线执行）、任务中心（节点级状态+产物下载）、实时日志、仪表盘（统计卡片+依赖检测）、深色/浅色主题切换、Toast 通知。

## Arch Linux 打包

打包文件位于 `packaging/` 目录：

```bash
cd packaging
makepkg -si
```

包含：PKGBUILD、entrypoint.desktop（桌面启动器）、entrypoint.service（systemd 服务）、entrypoint.install（安装钩子）。

> 注：`makepkg` 需在 Arch Linux 环境执行。RHEL/Fedora 上仅提供打包定义。

## 状态

✅ 核心功能已完成，WebUI + 桌面 GUI 反向移植已实现，E2E 全流程测试通过（2026-08-04）

- ✅ 288 个 Rust 测试全部通过，clippy 零警告
- ✅ 真实媒体 E2E：`video_to_srt` 管线全流程跑通（视频→音频提取→ASR large-v3→SRT 产物下载，GPU 不可用时自动 CPU 回退）
- ✅ 浏览器实测：WebUI 全部 8 个页面零控制台错误
- ✅ 模型获取三路径验证：在线下载（HuggingFace/ModelScope/URL 三源 + 镜像选源）、浏览器上传（文件夹/zip/tar.gz）、服务器本地路径导入

已知限制：

- 首次下载自动准备模块 venv（含 torch 等大型依赖）时约需 15–20 分钟，客户端超时需放宽或重试
- daemon 重启不回收模块子进程
