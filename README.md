# EntryPoint

EntryPoint是一款AI创意/内容处理工具管控平台，能够统一调度多种本地 AI 模型服务，支持多 GPU 分配与 DAG 管线工作流。

## 特性

- 🌐 WebUI 管理界面（React + TypeScript）— 浏览器访问即用（唯一 UI，桌面端已于 2026-08-13 退役）
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
- 📦 Arch Linux 打包 — PKGBUILD + systemd 服务

## 文档

- [设计文档](DESIGN.md)
- [部署指南](docs/DEPLOYMENT.md)
- [WebUI 使用指引](docs/WEBUI_GUIDE.md)
- [WebUI 设计系统](docs/DESIGN_SYSTEM.md)
- [模块接入规范](docs/MODULE_SPEC.md)
- [Adapter REST API 规范](docs/ADAPTER_API.md)
- [管线规范](docs/PIPELINE_SPEC.md)
- [自动化集成指南](docs/AUTOMATION.md)
- [整合包作者指南](docs/PACK_AUTHORING.md)
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

统一构建脚本（server 模式；桌面端已退役，无 gui 模式）：

### Windows

```powershell
.\build.ps1 server     # 服务器 zip（ep-daemon + WebUI + start-daemon.bat）
```

可选参数：`-Target debug|release`（默认 release）、`-SkipTest`、`-SkipClippy`、`-SkipFrontend`、`-Clean`、`-OutputDir <dir>`（默认 dist）。

### Linux

```bash
./build.sh server      # 服务器包（ZIP 主产物 + tar.gz 兜底 + deb/rpm/PKGBUILD）
```

- 可选参数：`-t debug|release`、`-d <distro>`（目标发行版 glibc/依赖适配）、`--skip-test`、`--skip-clippy`、`--skip-frontend`、`--clean`、`-o <dir>`

> **WebUI 前端自动构建**：打包时会自动构建 WebUI 前端（在 `crates/ep-webui/frontend` 执行 `npm ci && npm run build`，产物输出到 `crates/ep-webui/static`），需已安装 Node.js/npm。前端构建在 cargo 编译之前执行（fail-fast），npm 环境异常不会浪费整轮编译。可用 `-SkipFrontend`（build.ps1）/ `--skip-frontend`（build.sh）跳过构建，直接使用现有 static 产物。npm 缺失但已有 static 产物时降级为警告继续（产物可能陈旧）；static 产物缺失则直接报错退出，杜绝静默空包。
>
> **注意**：`crates/ep-webui/static/` 为 git 跟踪文件，且 vite 配置 `emptyOutDir`——打包自动重建会整体改写该目录。前端变更后应把更新后的 static 产物随仓库一并提交。

构建产物：
- 二进制：`target/release/ep-daemon`（服务器 daemon）
- 打包产物：`dist/` 下 **ZIP（主产物）** / tar.gz / deb / rpm / PKGBUILD

## Linux 部署指南（解压目录自包含）

ZIP 解压到任意目录（如 `/server/AnotherViewer`），一切运行在该目录内——
不复制到 /opt、不绑定发行版目录布局：

```bash
unzip EntryPoint-vX-linux-x86_64-server.zip
cd EntryPoint-vX-linux-x86_64-server
./deploy.sh install        # 交互式：依赖安装 → 配置向导 → systemd（不自启）→ 健康自检
```

- 支持发行版族：Debian/Ubuntu、Fedora、RHEL/CentOS/Rocky/Alma、Arch/Manjaro（自动探测）
- 非交互：`./deploy.sh install --yes`；运维子命令：`status / start / stop / logs / configure / check / uninstall`
- **模型权重不入包**：首次部署后通过 WebUI 模块页下载 / 上传 / 本地导入，
  或直接把权重目录放入 `<部署目录>/models/`
- systemd 服务按约定**不设置开机自启**；`systemctl stop/restart` 走 SIGTERM
  优雅回收（逐个停止模块子进程并释放端口）

> ⚠️ 安全提示：缺省仅监听 `127.0.0.1`。局域网/公网暴露、防火墙（firewalld/ufw）、
> SELinux、API token 建议等全部由 `deploy.sh install` 向导覆盖，
> 详见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)。

## Windows 快速开始（server 包）

1. 构建：`.\build.ps1 server`（或直接解压已发布的 zip）
2. 双击包内 `start-daemon.bat` — 自动拉起 ep-daemon 并打开默认浏览器访问 `http://127.0.0.1:9800`
3. 可选：`start-daemon.bat --no-browser` 跳过自动开浏览器（无人值守场景）

WebUI 为唯一 UI（桌面端已于 2026-08-13 退役，决策依据见 docs/DESKTOP_SUNSET_PLAN.md）。

## Arch Linux 打包

打包文件位于 `packaging/` 目录：

```bash
cd packaging
makepkg -si
```

包含：PKGBUILD（server）、entrypoint.service（systemd 服务）、entrypoint.install（安装钩子）。

> 注：`makepkg` 需在 Arch Linux 环境执行。RHEL/Fedora 上仅提供打包定义。

## 状态

✅ 核心功能已完成，WebUI 全流程测试通过；桌面端已于 2026-08-13 退役（WebUI 为唯一 UI）

- ✅ 1100+ Rust 测试全部通过，clippy 零警告
- ✅ 真实媒体 E2E：`video_to_srt` 管线全流程跑通（视频→音频提取→ASR large-v3→SRT 产物下载，GPU 不可用时自动 CPU 回退）
- ✅ Linux 真机（Arch）落地：ZIP 自包含部署（deploy.sh 交互安装 + systemd）、5 模块真实推理验证（rembg 抠图 / faster-whisper large-v3 GPU 转写 / paddleocr 识别 等）
- ✅ 浏览器实测：WebUI 全部页面零控制台错误
- ✅ 模型获取三路径验证：在线下载（HuggingFace/ModelScope/URL 三源 + 镜像选源）、浏览器上传（文件夹/zip/tar.gz）、服务器本地路径导入

已知限制：

- 首次自动准备模块 venv：轻依赖模块约 30 秒；torch 等大型依赖慢网下 10–30 分钟（失败自动拆除半壳 venv，重试即可）
- `kill -9` 强杀 daemon 不走优雅回收（正常 stop/restart 均逐个回收模块子进程并释放端口）
- 随包 5 模块均未声明 openvino 后端：NPU/iGPU 设备可检出，暂无模块消费
- systemd 部署（PrivateTmp）下输入文件须位于部署目录内（推荐 `workspace/uploads/`）
