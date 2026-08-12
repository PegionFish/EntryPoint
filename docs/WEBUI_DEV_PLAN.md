# EntryPoint WebUI 全面开发计划

> ⚠️ **Sunset 横幅（2026-08-13）**：本文档所述 **ep-desktop 桌面端已于 2026-08-13 退役**，WebUI 为唯一 UI（server 形态交付）。本页保留为历史记录，不再维护；详见 [DESKTOP_SUNSET_PLAN.md](DESKTOP_SUNSET_PLAN.md)。

> 版本：v1.2 | 日期：2026-07-28
>
> 本文档是 WebUI 实现 + Linux 适配 + 部署的完整开发规范，
> 供执行代理（含子代理）作为唯一工作参考。
>
> **部署场景：默认仅内网（LAN）访问，代码层屏蔽公网。**
> daemon 绑定 `0.0.0.0:9800`，但通过 IP 过滤中间件默认拒绝所有非 RFC 1918 / 非 loopback 请求（403）。
> 用户可在配置中设置 `allow_public = true` 手动开启公网访问，WebUI 切换时须弹出安全风险警告。
> 本项目不内置 HTTPS / 用户认证 / 反向代理。

---

## 目录

1. [项目现状](#1-项目现状)
2. [目标环境](#2-目标环境)
3. [技术决策](#3-技术决策)
4. [架构设计](#4-架构设计)
5. [统一设计规范](#5-统一设计规范)
6. [Linux 适配清单](#6-linux-适配清单)
7. [波次与子代理编排](#7-波次与子代理编排)
8. [各代理详细任务书](#8-各代理详细任务书)
9. [API 参考](#9-api-参考)
10. [验证标准](#10-验证标准)
11. [风险与注意事项](#11-风险与注意事项)

---

## 1. 项目现状

### 1.1 Crate 架构

| Crate | 类型 | 二进制名 | 状态 | 说明 |
|---|---|---|---|---|
| `ep-core` | lib | — | ✅ 完整 | 模块系统、管线引擎、进程管理、设备检测、配置、环境管理 |
| `ep-daemon` | bin | `ep-daemon` | ✅ 完整 | Axum HTTP/WS 服务器，端口 9800，静态文件服务 |
| `ep-desktop` | bin | `entrypoint` | ⚠️ 仅 Windows | egui/eframe 桌面 GUI，需 Linux 适配 |
| `ep-webui` | lib | — | ❌ 占位 | 仅一个 placeholder `index.html` |

### 1.2 已有 API 端点

**REST（全部挂载在 `/api/` 下）**：

| 方法 | 路径 | 状态 | 说明 |
|---|---|---|---|
| GET | `/api/health` | ✅ | `{"status":"ok","version":"0.1.0"}` |
| GET | `/api/devices` | ✅ | 计算设备列表（GPU/CPU） |
| GET | `/api/modules` | ✅ | 已发现模块 + 服务状态 |
| POST | `/api/modules/{id}/start` | ✅ | 启动模块（分配端口+设备+spawn 进程） |
| POST | `/api/modules/{id}/stop` | ✅ | 停止模块（kill 进程+释放端口） |
| GET | `/api/modules/{id}/status` | ✅ | 模块运行状态、端口、运行时长 |
| GET | `/api/modules/{id}/logs` | ✅ | 模块日志缓冲区（最近 500 行） |
| GET | `/api/config` | ✅ | 获取完整 AppConfig |
| PUT | `/api/config` | ✅ | 更新 AppConfig |
| GET | `/api/models` | ✅ | 所有模块的模型状态 |
| GET | `/api/models/{module_id}` | ✅ | 指定模块的模型详情（大小、文件数、缓存路径） |
| POST | `/api/models/{module_id}/import` | ✅ | 从本地路径导入模型 |
| GET | `/api/deps` | ✅ | 外部依赖检测报告（ffmpeg、torch CUDA） |
| GET | `/api/pipelines` | ❌ 占位 | 返回 `[]` |
| POST | `/api/pipelines/execute` | ❌ 占位 | 返回 "not yet implemented" |
| GET | `/api/pipelines/{id}/status` | ❌ 占位 | 返回 "unknown" |

**WebSocket**：

| 路径 | 消息类型 | 状态 |
|---|---|---|
| `/ws/logs` | `{"module_id":"...","line":"..."}` | ⚠️ 通道已建，但无数据源（见 §6.1） |
| `/ws/progress` | `{"pipeline_id":"...","node_id":"...","status":"..."}` | ⚠️ 同上 |

**静态文件**：daemon 通过 `tower_http::services::ServeDir` 将 `crates/ep-webui/static/` 作为 fallback 服务。

### 1.3 已有模块

| 模块 ID | 类别 | 运行时 | 计算后端 | 模型来源 |
|---|---|---|---|---|
| `faster-whisper` | asr | Python | cuda, rocm, cpu | HuggingFace (3 个模型) |
| `deep-filter` | denoise | Python | cuda, cpu | URL (DeepFilterNet3 ONNX) |
| `paddleocr` | ocr | Python | cuda, cpu | URL (PP-OCRv4) |
| `qwen3-tts` | tts | Python | cuda, cpu | ModelScope (2 个模型) |
| `rembg` | image | Python | cuda, cpu | URL (U2-Net/ISNet/BiRefNet) |
| `test-ffmpeg` | — | native | — | 仅 Windows .exe（git-ignored） |

每个 Python 模块包含：`module.toml`（清单）、`adapter.py`（FastAPI 适配器）、`requirements.txt`、`README.md`。

### 1.4 桌面端页面（egui，需在 WebUI 中重现并增强）

| 页面 | 功能 |
|---|---|
| 仪表盘 | 计算设备卡片（显存/利用率/温度）+ 模块状态表格 |
| 模块 | 按类别分组列表 + 详情面板（启停控制、日志查看器、状态信息） |
| 管线 | 加载 TOML 文件 → 验证 → 显示节点/边/拓扑层（无可视化编辑器） |
| 任务 | 运行中服务列表 + 全部模块状态 |
| 设置 | 完整配置编辑器（计算策略、端口、模型、Python、管线、UI） |

### 1.5 已知问题

1. **WebSocket 日志流未接通**：`process.rs` 中 `child.stdout.take()` / `child.stderr.take()` 丢弃了子进程输出，未管道到 `log_tx` broadcast channel
2. **Pipeline API 全部是占位**：管线执行引擎在 ep-core 中已完整实现，但 daemon 的 3 个管线端点未接入
3. **Git 工作区有 6 个 modified 文件**：仅 CRLF→LF 换行符变化，无语义改动
4. **大量 Windows 硬编码**：详见 §6

### 1.6 测试现状

- 131 个测试（111 unit + 13 integration + 7 daemon），全部在 Windows 上通过
- `process.rs` 中约 10 个测试使用 `cmd /C` 命令，在 Linux 上会失败
- 0 个 clippy 警告

---

## 2. 目标环境

### 2.1 服务器硬件与系统

| 项目 | 值 |
|---|---|
| OS | Red Hat Enterprise Linux 9.8 (Plow) |
| Kernel | 7.1.1-1.el9.elrepo.x86_64 (PREEMPT_DYNAMIC) |
| CPU | 8 核 |
| 内存 | 16 GB |
| GPU | NVIDIA Tesla P4 8GB (GP104GL) |
| CUDA | 13.0 |
| 驱动 | 580.159.04 |
| 磁盘 | 559GB 总量，432GB 可用（/server 分区） |

### 2.2 已安装工具

| 工具 | 版本 | 备注 |
|---|---|---|
| Node.js | v20.20.2 | ✅ 可用于前端构建 |
| npm | 10.8.2 | ✅ |
| Python | 3.9.25 | ⚠️ 模块要求 >=3.10，需 uv 代管 |
| Git | 2.52.0 | ✅ |
| curl | 7.76.1 | ✅ |
| Rust | ❌ 未安装 | 需要 rustup |
| uv | ❌ 未安装 | 需要安装 |
| ffmpeg | ❌ 未安装 | 需要 dnf 或静态构建 |

### 2.3 需要安装

```bash
# Rust 工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# uv（Python 包管理器）
curl -LsSf https://astral.sh/uv/install.sh | sh

# ffmpeg（二选一）
sudo dnf install ffmpeg-free          # RHEL EPEL 仓库
# 或下载静态构建：https://johnvansickle.com/ffmpeg/
```

---

## 3. 技术决策

### 3.1 决策记录

| 决策项 | 选择 | 理由 |
|---|---|---|
| 前端框架 | React 18 + TypeScript + Vite 6 | React Flow (DAG 编辑器) 生态最成熟 |
| UI 组件库 | shadcn/ui + TailwindCSS 4 | 可定制、无运行时依赖、暗色主题原生支持 |
| DAG 编辑器 | @xyflow/react (React Flow) | 业界标准 DAG 可视化库 |
| 状态管理 | Zustand | 轻量、TypeScript 友好、无 boilerplate |
| 路由 | React Router 7 | 标准选择 |
| 桌面端 | 保留 egui，同步适配 Linux | 用户决策 |
| UI 统一 | 共享设计规范文档 `docs/DESIGN_SYSTEM.md` | 两套实现遵循同一规范 |
| 语言 | 仅中文 | 当前所有 UI 文本均为中文 |
| Python 环境 | 安装 uv 代管，不立即重建 venv | 服务器 Python 3.9 不满足要求 |
| 部署 | systemd 开机自启 | 标准 Linux 服务管理 |
| Git 策略 | 每个代理完成后自行 commit，不 push | 未配置 GitHub 访问权限 |

### 3.2 前端依赖清单

```json
{
  "dependencies": {
    "react": "^18",
    "react-dom": "^18",
    "react-router-dom": "^7",
    "@xyflow/react": "^12",
    "zustand": "^5",
    "lucide-react": "latest",
    "clsx": "latest",
    "tailwind-merge": "latest"
  },
  "devDependencies": {
    "typescript": "^5",
    "vite": "^6",
    "@vitejs/plugin-react": "latest",
    "tailwindcss": "^4",
    "@tailwindcss/vite": "latest"
  }
}
```

shadcn/ui 通过 `npx shadcn@latest init` 初始化，按需添加组件（button, card, badge, dialog, toast, table, tabs, input, select, switch, separator, scroll-area, tooltip, dropdown-menu, sheet, skeleton, progress）。

---

## 4. 架构设计

### 4.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                      用户浏览器                              │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  React SPA (Vite 构建)                                │  │
│  │  ┌─────┐ ┌─────┐ ┌──────┐ ┌────┐ ┌────┐ ┌────┐     │  │
│  │  │仪表 │ │模块 │ │管线  │ │任务│ │模型│ │设置│     │  │
│  │  │盘   │ │管理 │ │编辑器│ │中心│ │管理│ │    │     │  │
│  │  └──┬──┘ └──┬──┘ └──┬───┘ └─┬──┘ └─┬──┘ └─┬──┘     │  │
│  │     └───────┴───────┴───────┴──────┴──────┘          │  │
│  │                    │                                  │  │
│  │         ┌──────────┴──────────┐                       │  │
│  │         │  API Client Layer   │                       │  │
│  │         │  (fetch + WS hooks) │                       │  │
│  │         └──────────┬──────────┘                       │  │
│  └────────────────────┼──────────────────────────────────┘  │
└───────────────────────┼─────────────────────────────────────┘
                        │ HTTP / WebSocket
                        ▼
┌───────────────────────────────────────────────────────────────┐
│  ep-daemon (Axum, port 9800)                                  │
│  ┌─────────────┐  ┌────────────┐  ┌────────────────────────┐ │
│  │ REST API    │  │ WebSocket  │  │ ServeDir (static/)     │ │
│  │ /api/*      │  │ /ws/logs   │  │ → React 构建产物       │ │
│  │             │  │ /ws/progress│  │                        │ │
│  └──────┬──────┘  └─────┬──────┘  └────────────────────────┘ │
│         └───────────────┘                                     │
│                  │                                            │
│         ┌───────┴────────┐                                    │
│         │   AppState     │                                    │
│         │ (Arc<RwLock>)  │                                    │
│         └───────┬────────┘                                    │
└─────────────────┼─────────────────────────────────────────────┘
                  │
         ┌───────┴────────┐
         │    ep-core     │
         │ ┌────────────┐ │
         │ │ProcessMgr  │ │──→ 子进程（Python 模块适配器）
         │ │PortMgr     │ │
         │ │ModuleSystem│ │
         │ │PipelineEng │ │
         │ │ComputeMgr  │ │──→ nvidia-smi
         │ │ConfigStore │ │
         │ │ModelMgr    │ │
         │ │EnvManager  │ │──→ uv / python3
         │ └────────────┘ │
         └────────────────┘
```

### 4.2 前端目录结构

```
crates/ep-webui/
├── Cargo.toml              # 不变（无 Rust 依赖）
├── src/lib.rs              # 不变（空）
├── frontend/               # ← 新建：React 项目根目录
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── tsconfig.app.json
│   ├── tsconfig.node.json
│   ├── index.html
│   ├── src/
│   │   ├── main.tsx        # 入口：ReactDOM.createRoot
│   │   ├── App.tsx         # 路由定义 + 全局布局
│   │   ├── index.css       # TailwindCSS 入口 + 全局样式
│   │   │
│   │   ├── api/            # API 通信层
│   │   │   ├── client.ts   # fetch 封装（baseURL、错误处理）
│   │   │   ├── types.ts    # 所有 API 请求/响应 TypeScript 类型
│   │   │   └── ws.ts       # WebSocket 连接管理（自动重连）
│   │   │
│   │   ├── hooks/          # 自定义 React Hooks
│   │   │   ├── use-devices.ts      # 轮询 /api/devices
│   │   │   ├── use-modules.ts      # 模块列表 + 操作
│   │   │   ├── use-module-detail.ts # 单模块状态/日志
│   │   │   ├── use-config.ts       # 配置读写
│   │   │   ├── use-models.ts       # 模型状态
│   │   │   ├── use-deps.ts         # 依赖检测
│   │   │   ├── use-websocket.ts    # 通用 WS hook
│   │   │   └── use-toast.ts        # Toast 通知
│   │   │
│   │   ├── stores/         # Zustand 状态管理
│   │   │   └── app-store.ts        # 全局状态（主题、WS 连接状态）
│   │   │
│   │   ├── components/     # 组件
│   │   │   ├── ui/         # shadcn/ui 基础组件（npx shadcn 生成）
│   │   │   ├── layout/     # 布局组件
│   │   │   │   ├── sidebar.tsx     # 左侧导航栏
│   │   │   │   ├── header.tsx      # 顶部栏（标题、连接状态、主题切换）
│   │   │   │   └── page-container.tsx # 页面容器（标题、面包屑）
│   │   │   └── shared/     # 业务共享组件
│   │   │       ├── status-badge.tsx    # 状态徽章（Running/Stopped/Error/...）
│   │   │       ├── device-card.tsx     # 计算设备卡片
│   │   │       ├── log-viewer.tsx      # 日志查看器（自动滚动、行号）
│   │   │       ├── module-card.tsx     # 模块摘要卡片
│   │   │       ├── confirm-dialog.tsx  # 确认对话框
│   │   │       ├── empty-state.tsx     # 空状态占位
│   │   │       └── loading-skeleton.tsx # 加载骨架屏
│   │   │
│   │   ├── pages/          # 页面组件
│   │   │   ├── dashboard.tsx       # 仪表盘
│   │   │   ├── modules.tsx         # 模块管理（列表）
│   │   │   ├── module-detail.tsx   # 模块详情（可作为侧面板或独立页）
│   │   │   ├── pipeline-editor.tsx # 管线编辑器（React Flow）
│   │   │   ├── tasks.tsx           # 任务中心
│   │   │   ├── models.tsx          # 模型管理
│   │   │   └── settings.tsx        # 设置
│   │   │
│   │   └── lib/            # 工具函数
│   │       ├── utils.ts    # cn()、格式化函数
│   │       └── constants.ts # 类别标签映射、状态颜色映射
│   │
│   └── components.json     # shadcn/ui 配置
│
└── static/                 # ← Vite 构建输出（daemon ServeDir 服务）
    ├── index.html          # 构建后的入口
    └── assets/             # JS/CSS/图片
```

### 4.3 Vite 配置要点

```typescript
// vite.config.ts
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  build: {
    outDir: '../static',     // 输出到 daemon 静态文件目录
    emptyOutDir: true,       // 构建前清空
  },
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://localhost:9800',   // 开发时代理 API
      '/ws': {
        target: 'ws://localhost:9800',
        ws: true,
      },
    },
  },
})
```

### 4.4 Daemon SPA Fallback 改造

当前 daemon 使用 `ServeDir` 作为 fallback，但 SPA 的 client-side routing（如 `/modules/faster-whisper`）会返回 404。需要在 Agent J (Integrator) 阶段修改 `ep-daemon/src/main.rs`：

```rust
// 非 /api/* 和 /ws/* 的路径全部返回 index.html（SPA history mode）
// 方案：使用 tower_http 的 ServeDir + 自定义 fallback
.fallback_service(
    ServeDir::new("crates/ep-webui/static")
        .not_found_service(ServeFile::new("crates/ep-webui/static/index.html"))
)
```

### 4.5 服务器配置与公网访问控制

#### 配置新增

`config/app.toml` 新增 `[server]` 段（`ep-core/src/config.rs` 的 `AppConfig` 对应新增字段）：

```toml
[server]
host = "0.0.0.0"       # 监听地址
port = 9800             # 监听端口
allow_public = false    # 是否允许公网 IP 访问（默认 false = 仅内网）
```

#### IP 过滤中间件（ep-daemon）

在 `ep-daemon/src/main.rs` 的路由构建中添加 axum 中间件层：

```rust
use std::net::{IpAddr, SocketAddr};
use axum::extract::ConnectInfo;

/// 判断 IP 是否为私有/本地地址（RFC 1918 + loopback + link-local）
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// IP 过滤中间件：allow_public = false 时拒绝非私有 IP
async fn ip_filter(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let allow_public = {
        let config = state.config.read().await;
        config.server.allow_public
    };
    if !allow_public && !is_private_ip(&addr.ip()) {
        tracing::warn!(ip = %addr.ip(), "blocked public access attempt");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}
```

路由挂载：

```rust
let app = Router::new()
    .merge(api::api_router())
    .merge(ws::ws_router())
    .fallback_service(/* SPA fallback */)
    .layer(axum::middleware::from_fn_with_state(state.clone(), ip_filter))
    .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
    .with_state(state);
```

注意：`into_make_service()` 需改为 `into_make_service_with_connect_info::<SocketAddr>()` 以获取客户端 IP。

#### 启动日志

```rust
if config.server.allow_public {
    tracing::warn!("public access ENABLED — no built-in auth/encryption, use at your own risk");
} else {
    tracing::info!("public access blocked — only private/loopback IPs allowed (set allow_public=true to change)");
}
```

#### WebUI 设置页交互

Settings 页面的「服务器」区域：
- host / port 输入框
- 「允许公网访问」开关（Switch 组件）
- 开启时弹出确认对话框：
  > ⚠️ 安全风险警告
  >
  > 开启后，任何能访问此服务器 IP 的设备均可操作 EntryPoint，包括启停模块、修改配置。
  > 本项目不内置用户认证和传输加密。
  >
  > 仅在您了解风险并有外部安全措施（如 VPN、防火墙规则）时开启。
- 确认后写入配置（PUT /api/config），daemon 热生效（下次请求即检查新值）

#### 实现归属

| 任务 | 代理 |
|---|---|
| `AppConfig` 新增 `server` 段 + 默认值 | Agent A (LinuxCore) |
| IP 过滤中间件 + 启动日志 | Agent A (LinuxCore) |
| `into_make_service_with_connect_info` | Agent A (LinuxCore) |
| WebUI 设置页服务器区域 + 警告对话框 | Agent H (PageTaskSettings) |
| 集成验证（公网 IP 403、内网 IP 200） | Agent K (Tester) |

---

## 5. 统一设计规范

### 5.1 原则

- `docs/DESIGN_SYSTEM.md` 是 WebUI (React) 和桌面端 (egui) 的**共同 UI/UX 真相源**
- 任何 UI/UX 改动流程：**先更新 DESIGN_SYSTEM.md → 再分别实现到 React 和 egui**
- Wave 1 Agent B 创建初始版本，Wave 4 Agent L 负责双向同步

### 5.2 DESIGN_SYSTEM.md 初始内容要求

Agent B 在搭建前端骨架时须创建 `docs/DESIGN_SYSTEM.md`，至少涵盖：

#### 5.2.1 配色方案

```
暗色主题（默认）：
  背景色：      hsl(240, 10%, 3.9%)    — 页面底色
  卡片背景：    hsl(240, 6%, 10%)      — 卡片/面板
  边框：        hsl(240, 4%, 16%)      — 分隔线/边框
  主文本：      hsl(0, 0%, 98%)        — 标题/正文
  次要文本：    hsl(240, 5%, 65%)      — 说明/标签
  主题色：      hsl(217, 91%, 60%)     — 按钮/链接/高亮（蓝色）
  
状态色：
  Running：     hsl(142, 71%, 45%)     — 绿色
  Stopped：     hsl(240, 5%, 65%)      — 灰色
  Starting：    hsl(217, 91%, 60%)     — 蓝色（同主题色）
  Preparing：   hsl(45, 93%, 47%)      — 黄色
  Error：       hsl(0, 84%, 60%)       — 红色
  NotReady：    hsl(240, 5%, 45%)      — 深灰

亮色主题：
  （shadcn/ui 默认亮色方案，通过 CSS 变量切换）
```

#### 5.2.2 排版

```
字体栈：  system-ui, -apple-system, "Noto Sans SC", "Microsoft YaHei", sans-serif
等宽字体：ui-monospace, "Cascadia Code", "Fira Code", "Noto Sans Mono CJK SC", monospace

层级：
  页面标题：  text-2xl font-bold    (24px)
  区块标题：  text-lg font-semibold (18px)
  卡片标题：  text-base font-medium (16px)
  正文：      text-sm               (14px)
  辅助文本：  text-xs text-muted    (12px)
  日志文本：  text-xs font-mono     (12px 等宽)
```

#### 5.2.3 布局模式

```
整体布局：
  ┌──────────────────────────────────────────┐
  │ Header (h-14, 固定顶部)                   │
  ├────────┬─────────────────────────────────┤
  │Sidebar │ Content Area                    │
  │(w-56)  │  ┌─────────────────────────┐    │
  │固定左侧│  │ PageContainer           │    │
  │        │  │  - 页面标题             │    │
  │ 导航项 │  │  - 操作按钮区           │    │
  │        │  │  - 内容区（可滚动）     │    │
  │        │  └─────────────────────────┘    │
  └────────┴─────────────────────────────────┘

卡片：    rounded-xl border p-6 shadow-sm
表格：    striped, hover 高亮
详情面板：右侧 Sheet (抽屉) 或 Dialog (模态)
```

#### 5.2.4 组件行为规约

| 组件 | 行为规范 |
|---|---|
| 状态徽章 | 圆点 + 文字，颜色对应状态色，Running 时圆点带 pulse 动画 |
| 启动/停止按钮 | 启动=绿色 outline，停止=红色 destructive；操作中显示 spinner + 禁用 |
| 日志查看器 | 等宽字体，行号，自动滚动到底部（可锁定/解锁），最大高度 400px，深色背景 |
| Toast 通知 | 右上角，自动消失 5s，成功=绿/错误=红/警告=黄 |
| 确认对话框 | 破坏性操作（停止模块、删除）必须确认 |
| 空状态 | 居中图标 + 说明文字 + 操作引导 |
| 加载状态 | 首次加载用骨架屏，后续刷新用 subtle spinner |
| 设备卡片 | 显存进度条（<70% 绿，70-90% 黄，>90% 红），利用率，温度 |

#### 5.2.5 交互模式

| 场景 | 规范 |
|---|---|
| 设备状态刷新 | 轮询 `/api/devices`，间隔 3s |
| 模块列表刷新 | 轮询 `/api/modules`，间隔 5s |
| 模块日志 | WebSocket `/ws/logs` 实时推送 + REST 拉取历史 |
| 管线进度 | WebSocket `/ws/progress` 实时推送 |
| 操作反馈 | 所有 API 调用后立即 Toast 反馈（成功/失败） |
| 错误引导 | 错误信息包含具体原因 + 建议操作 |
| 自动刷新 | 页面可见时刷新，不可见时暂停（`document.visibilitychange`） |

#### 5.2.6 类别标签映射

```typescript
const CATEGORY_LABELS: Record<string, string> = {
  asr:       '语音识别',
  tts:       '语音合成',
  denoise:   '降噪',
  ocr:       '文字识别',
  image:     '图像处理',
  translate: '翻译',
  video:     '视频处理',
  face:      '人脸识别',
  custom:    '自定义',
}
```

#### 5.2.7 页面清单与路由

| 路由 | 页面 | 导航图标 | 导航标签 |
|---|---|---|---|
| `/` | 仪表盘 | `LayoutDashboard` | 仪表盘 |
| `/modules` | 模块管理 | `Puzzle` | 模块 |
| `/modules/:id` | 模块详情 | — | （从模块列表进入） |
| `/pipeline` | 管线编辑器 | `GitBranch` | 管线 |
| `/tasks` | 任务中心 | `ListTodo` | 任务 |
| `/models` | 模型管理 | `Database` | 模型 |
| `/settings` | 设置 | `Settings` | 设置 |

图标库：lucide-react。

---

## 6. Linux 适配清单

### 6.1 高优先级（运行时会崩溃）

| # | 文件 | 问题 | 修复方案 |
|---|---|---|---|
| 1 | `ep-core/src/pipeline/executor.rs` | `resolve_ffmpeg_path()` 3 处硬编码 `ffmpeg.exe` | 使用 `cfg!(windows)` 分支：Windows 用 `ffmpeg.exe`，Linux 用 `ffmpeg` |
| 2 | `ep-core/src/deps.rs` | 6 处硬编码 `ffmpeg.exe`；Linux fallback 路径列表为空 | 同上 + 添加 `/usr/bin/ffmpeg`、`/usr/local/bin/ffmpeg` 等 Linux 候选路径 |
| 3 | `ep-core/src/deps.rs:213` | `check_all_deps()` 硬编码 `Scripts/python.exe` | 改用 `env.rs` 中已有的 `venv_python_path()` 函数（已正确分支） |
| 4 | `modules/*/module.toml` (×5) | `start_command` 硬编码 `Scripts/python.exe` | 改为 `bin/python`；或更好的方案：让 Rust 代码在构建命令时自动替换（`env.rs` 的 `venv_python_path()` 已实现） |
| 5 | `config/app.toml` | `cache_paths = ["D:/AI_Applications"]` | 改为 `cache_paths = []` |
| 6 | `ep-core/src/process.rs` 测试 | ~10 个测试使用 `cmd /C echo/timeout` | 改为 `cfg!(windows)` 分支：Windows 用 `cmd /C`，Linux 用 `sh -c` + `echo`/`sleep` |

### 6.2 中优先级（功能缺陷）

| # | 文件 | 问题 | 修复方案 |
|---|---|---|---|
| 7 | `ep-core/src/process.rs:128` | `child.stdout.take()` / `child.stderr.take()` 丢弃输出 | 改为 `BufReader` 逐行读取 → 写入 `log_buffer` + 发送到 `log_tx` broadcast channel |
| 8 | `ep-core/src/deps.rs:114` | 用户引导文本引用 `winget install ffmpeg` | 添加 Linux 引导：`sudo dnf install ffmpeg-free` |
| 9 | `ep-desktop/src/main.rs:58-63` | CJK 字体仅尝试 `C:\Windows\Fonts\*` | 添加 Linux 字体路径：`/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc` 等 |
| 10 | `ep-daemon/src/api/pipelines.rs` | 3 个端点全部是占位 | 接入 ep-core 的 PipelineRunnerImpl（可作为后续迭代） |

### 6.3 已正确处理（无需修改）

| 文件 | 说明 |
|---|---|
| `ep-core/src/env.rs` `venv_python_path()` | 已正确分支 `Scripts/python.exe` vs `bin/python` |
| `ep-core/src/process.rs` `start_module()` | 已正确分支 `cmd /C` vs `sh -c` |
| `ep-core/src/compute/cpu.rs` | 已正确分支 Win32 API vs `/proc/meminfo` |
| `ep-core/src/env.rs` `which()` | 已正确在 Windows 追加 `.exe` |
| `ep-core/src/module/lifecycle.rs` 测试 | 已正确用 `cfg!(windows)` 分支 |
| `modules/faster-whisper/adapter.py` DLL 路径 | 已正确用 `sys.platform == "win32"` 守卫 |

### 6.4 Windows 遗留文件（清理/忽略）

| 路径 | 处理 |
|---|---|
| `runtime/bin/ffmpeg.exe`, `ffprobe.exe` | git-ignored，不删除，安装 Linux ffmpeg |
| `runtime/venvs/*` | git-ignored Windows venv，后续用 uv 重建 |
| `modules/test-ffmpeg/*.exe` | git-ignored，不删除 |
| `dist/*` | git-ignored Windows 构建产物 |
| `build.ps1` | 保留（Windows 用户可能用到），新增 `scripts/build.sh` |
| `EntryPoint.zip` | 未跟踪，可删除 |

---

## 7. 波次与子代理编排

### 7.1 总览

```
Wave 0 ──→ Wave 1 (×4 并行) ──→ Wave 2 (×5 并行) ──→ Wave 3 (×2 并行) ──→ Wave 4 (×2 并行)
  1 代理       4 代理                5 代理               2 代理               2 代理
  环境准备     基础并行              UI 页面并行           集成测试             打磨文档
```

| 波次 | 代理数 | 并行度 | 前置条件 |
|---|---|---|---|
| Wave 0 | 1 | 1 | 无 |
| Wave 1 | 4 | 4 | Wave 0 完成 |
| Wave 2 | 5 | 5 | Wave 1 Agent B 完成（Agent A 最好也完成） |
| Wave 3 | 2 | 2 | Wave 2 全部完成 |
| Wave 4 | 2 | 2 | Wave 3 完成 |
| **总计** | **14** | **峰值 5** | |

### 7.2 依赖关系图

```
Wave 0 (EnvSetup)
    │
    ├──→ Wave 1A (LinuxCore) ──────────────────────┐
    ├──→ Wave 1B (WebScaffold + DESIGN_SYSTEM) ──┐ │
    ├──→ Wave 1C (Deploy)                        │ │
    └──→ Wave 1D (DesktopLinux)                  │ │
                                                 ▼ ▼
                          Wave 2 (5 个 UI 代理并行，遵循 DESIGN_SYSTEM.md)
                          ├── 2E (Dashboard)
                          ├── 2F (Modules)
                          ├── 2G (PipelineEditor)
                          ├── 2H (Tasks+Settings+Models)
                          └── 2I (SharedUX)
                                 │
                                 ▼
                          Wave 3 (集成 + 测试)
                          ├── 3J (Integrator)
                          └── 3K (Tester)
                                 │
                                 ▼
                          Wave 4 (打磨 + 文档)
                          ├── 4L (UXPolish → 双向同步 egui)
                          └── 4M (Docs)
```

### 7.3 文件写入隔离

**关键原则：同一波次内，任何两个代理的写入范围不得重叠。**

| 代理 | 独占写入范围 |
|---|---|
| 1A LinuxCore | `crates/ep-core/`, `crates/ep-daemon/src/api/`, `modules/*/module.toml`, `config/app.toml` |
| 1B WebScaffold | `crates/ep-webui/frontend/` (新建), `docs/DESIGN_SYSTEM.md` (新建) |
| 1C Deploy | `scripts/` (新建), `entrypoint.service` (新建), `.gitignore` |
| 1D DesktopLinux | `crates/ep-desktop/` |
| 2E Dashboard | `frontend/src/pages/dashboard.tsx`, `frontend/src/components/shared/device-card.tsx` |
| 2F Modules | `frontend/src/pages/modules.tsx`, `module-detail.tsx`, `frontend/src/components/shared/module-card.tsx`, `log-viewer.tsx` |
| 2G Pipeline | `frontend/src/pages/pipeline-editor.tsx`, `frontend/src/components/shared/pipeline-*.tsx` |
| 2H Tasks+Settings | `frontend/src/pages/tasks.tsx`, `settings.tsx`, `models.tsx` |
| 2I SharedUX | `frontend/src/components/layout/`, `frontend/src/hooks/`, `frontend/src/stores/`, `frontend/src/lib/`, `frontend/src/components/shared/` (status-badge, confirm-dialog, empty-state, loading-skeleton, toast) |
| 3J Integrator | `crates/ep-daemon/src/main.rs` (SPA fallback), `frontend/` (构建修复) |
| 3K Tester | `reports/` (测试报告) |
| 4L UXPolish | `frontend/src/` (全局优化), `crates/ep-desktop/` (egui 同步), `docs/DESIGN_SYSTEM.md` (回写) |
| 4M Docs | `README.md`, `PROGRESS.md`, `DESIGN.md`, `docs/` |

**共享只读文件**（Wave 2 代理可读取但不可修改）：
- `frontend/src/api/types.ts` — Agent B 定义，Wave 2 代理只读
- `frontend/src/api/client.ts` — 同上
- `docs/DESIGN_SYSTEM.md` — Agent B 创建，Wave 2 代理只读遵循

**冲突解决**：如果 Wave 2 代理需要新增共享组件（如新的 hook），应放在自己的页面文件内作为局部实现，由 Wave 3 Agent J 统一提取。

---

## 8. 各代理详细任务书

### 8.0 Wave 0 — Agent 0 (EnvSetup)

**目标**：准备可编译的 Rust + Node + Python 环境。

**任务**：
1. 安装 Rust 工具链：
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source "$HOME/.cargo/env"
   rustc --version && cargo --version
   ```
2. 安装 uv：
   ```bash
   curl -LsSf https://astral.sh/uv/install.sh | sh
   uv --version
   ```
3. 安装 ffmpeg（优先 dnf，备选静态构建）：
   ```bash
   sudo dnf install -y epel-release
   sudo dnf install -y ffmpeg-free
   ffmpeg -version
   ```
4. 验证项目可编译（可能有测试失败，记录但不修复）：
   ```bash
   cd /server/EntryPoint
   cargo check 2>&1 | tail -20
   ```
5. 处理 CRLF 换行符问题：
   ```bash
   git add -A && git commit -m "chore: normalize line endings (CRLF → LF)"
   ```
6. 清理未跟踪文件：
   ```bash
   rm -f EntryPoint.zip
   ```

**验证标准**：
- `rustc --version` 输出 stable 版本
- `uv --version` 输出版本
- `ffmpeg -version` 输出版本
- `cargo check` 成功（或仅有 process.rs 测试相关的已知问题）

**commit**：`chore(wave-0): install toolchain + normalize line endings`

---

### 8.1 Wave 1 — Agent A (LinuxCore)

**目标**：修复所有 Rust 代码和配置中的 Windows 遗留问题，使 `cargo test` 在 Linux 上全部通过。

**写入范围**：`crates/ep-core/`, `crates/ep-daemon/src/api/`, `modules/*/module.toml`, `config/app.toml`

**任务**：

1. **`executor.rs` — `resolve_ffmpeg_path()`**：
   - 3 处 `ffmpeg.exe` → 使用条件编译或运行时检测
   ```rust
   let ffmpeg_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
   ```
   - 搜索路径 `runtime/bin/ffmpeg.exe` → `runtime/bin/{ffmpeg_name}`
   - 搜索路径 `modules/test-ffmpeg/ffmpeg.exe` → `modules/test-ffmpeg/{ffmpeg_name}`

2. **`deps.rs`**：
   - 所有 `ffmpeg.exe` 硬编码 → 同上条件编译
   - `#[cfg(not(windows))]` 块添加 Linux 候选路径：
     ```rust
     candidates.push("/usr/bin/ffmpeg".into());
     candidates.push("/usr/local/bin/ffmpeg".into());
     candidates.push("/snap/bin/ffmpeg".into());
     ```
   - `check_all_deps()` 中 `Scripts/python.exe` → 改用 `env::venv_python_path()`
   - 用户引导文本添加 Linux 安装命令

3. **`process.rs` 测试**：
   - 所有 `cmd /C echo hello` → 条件分支：
     ```rust
     let cmd = if cfg!(windows) { "cmd /C echo hello" } else { "echo hello" };
     let cmd = if cfg!(windows) { "cmd /C timeout /t 30" } else { "sleep 30" };
     ```
   - 约 10 处需要修改

4. **`modules/*/module.toml` (×5)**：
   - `start_command` 中 `Scripts/python.exe` → `bin/python`
   - 注意：这只在 Linux 上正确。更好的长期方案是让 Rust 代码在运行时根据平台构建路径，但当前先改为 Linux 格式（项目已迁移到 Linux 服务器）

5. **`config/app.toml`**：
   - `cache_paths = ["D:/AI_Applications"]` → `cache_paths = []`

6. **（可选但推荐）`process.rs` 日志流接通**：
   - 将 `child.stdout.take()` / `child.stderr.take()` 改为 `BufReader` 逐行读取
   - 读取的行写入 `instance.log_buffer` + 发送到 `log_tx` broadcast channel
   - 这需要 `ProcessManager` 持有 `log_tx` 的引用，或在 `start_module` 时传入
   - 如果改动过大，可留给 Wave 3 Agent J

7. **`config.rs` — 新增 `[server]` 配置段**：
   - `AppConfig` 新增 `server: ServerConfig` 字段
   - `ServerConfig { host: String, port: u16, allow_public: bool }`
   - 默认值：`host = "0.0.0.0"`, `port = 9800`, `allow_public = false`
   - `config/app.toml` 添加对应段

8. **`ep-daemon/src/main.rs` — IP 过滤中间件 + 配置化监听地址**：
   - 实现 `is_private_ip()` + `ip_filter` 中间件（完整代码见 §4.5）
   - 监听地址从硬编码 `([0, 0, 0, 0], 9800)` 改为读取 `config.server.host` / `config.server.port`
   - `into_make_service()` → `into_make_service_with_connect_info::<SocketAddr>()`
   - 启动日志：`allow_public = false` 时 INFO，`true` 时 WARN（见 §4.5）

**验证**：
```bash
cargo test 2>&1 | tail -5    # 所有测试通过
cargo clippy 2>&1 | tail -5  # 0 警告
```

**commit**：`fix(wave-1a): linux adaptation + server config + IP filter middleware`

---

### 8.2 Wave 1 — Agent B (WebScaffold)

**目标**：搭建完整的 React 前端项目骨架 + 创建统一设计规范文档。

**写入范围**：`crates/ep-webui/frontend/` (新建), `docs/DESIGN_SYSTEM.md` (新建)

**任务**：

1. **初始化 React 项目**：
   ```bash
   cd /server/EntryPoint/crates/ep-webui
   npm create vite@latest frontend -- --template react-ts
   cd frontend
   npm install
   ```

2. **安装依赖**：
   ```bash
   npm install react-router-dom @xyflow/react zustand lucide-react clsx tailwind-merge
   npm install -D tailwindcss @tailwindcss/vite
   ```

3. **初始化 shadcn/ui**：
   ```bash
   npx shadcn@latest init
   # 选择：TypeScript, src/ 目录, @/ alias, CSS variables
   npx shadcn@latest add button card badge dialog table tabs input select \
     switch separator scroll-area tooltip dropdown-menu sheet skeleton \
     progress sonner
   ```

4. **配置 `vite.config.ts`**：（见 §4.3）

5. **创建 API 通信层**：
   - `src/api/types.ts`：根据 §9 API 参考定义所有 TypeScript 接口
   - `src/api/client.ts`：fetch 封装（baseURL 自动检测、JSON 解析、错误处理）
   - `src/api/ws.ts`：WebSocket 连接管理器（自动重连、心跳、JSON 解析）

6. **创建布局组件**：
   - `src/components/layout/sidebar.tsx`：左侧导航（图标 + 标签，见 §5.2.7 路由表）
   - `src/components/layout/header.tsx`：顶部栏（应用名、WS 连接状态指示、主题切换）
   - `src/components/layout/page-container.tsx`：页面容器（标题、描述、操作区）

7. **创建路由骨架**：
   - `src/App.tsx`：React Router 配置，7 个路由对应 7 个页面占位组件
   - 每个页面占位：`<PageContainer title="..."><EmptyState /></PageContainer>`

8. **配置全局样式**：
   - `src/index.css`：TailwindCSS 入口 + shadcn/ui CSS 变量（暗色/亮色主题）
   - 默认暗色主题

9. **创建 `docs/DESIGN_SYSTEM.md`**：
   - 按 §5.2 的内容要求编写完整的设计规范
   - 这是所有后续 UI 代理的参考文档

10. **验证**：
    ```bash
    npm run dev     # 开发服务器启动，页面可访问
    npm run build   # 构建成功，输出到 ../static/
    ```

**commit**：`feat(wave-1b): scaffold React WebUI + design system document`

---

### 8.3 Wave 1 — Agent C (Deploy)

**目标**：创建 Linux 部署基础设施。

**写入范围**：`scripts/` (新建), 项目根目录

**任务**：

1. **`scripts/entrypoint.service`**（systemd unit 文件）：
   ```ini
   [Unit]
   Description=EntryPoint AI Module Daemon
   After=network.target

   [Service]
   Type=simple
   User=bob
   WorkingDirectory=/server/EntryPoint
   ExecStart=/server/EntryPoint/target/release/ep-daemon
   Restart=on-failure
   RestartSec=5
   Environment=RUST_LOG=ep_daemon=info,ep_core=info
   # 确保 CUDA 库可被找到
   Environment=LD_LIBRARY_PATH=/usr/local/cuda/lib64
   StandardOutput=journal
   StandardError=journal
   SyslogIdentifier=entrypoint

   [Install]
   WantedBy=multi-user.target
   ```

2. **`scripts/install-service.sh`**：
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   sudo cp scripts/entrypoint.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable entrypoint
   echo "Service installed. Start with: sudo systemctl start entrypoint"
   ```

3. **`scripts/build.sh`**（替代 build.ps1）：
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   # 1. cargo build --release
   # 2. cd crates/ep-webui/frontend && npm ci && npm run build
   # 3. 打包到 dist/（可选）
   ```

4. **`scripts/start.sh`**（开发/手动启动）：
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   cd "$(dirname "$0")/.."
   export RUST_LOG="${RUST_LOG:-ep_daemon=info,ep_core=info}"
   exec ./target/release/ep-daemon
   ```

5. **更新 `.gitignore`**：
   - 添加 `node_modules/`（如果还没有）
   - 添加 `crates/ep-webui/frontend/node_modules/`

6. **设置可执行权限**：
   ```bash
   chmod +x scripts/*.sh
   ```

**commit**：`feat(wave-1c): add systemd service, build script, start script for Linux`

---

### 8.4 Wave 1 — Agent D (DesktopLinux)

**目标**：使 ep-desktop 在 Linux 上可编译，修复 CJK 字体路径。

**写入范围**：`crates/ep-desktop/`

**任务**：

1. **`src/main.rs` CJK 字体路径**：
   - 在现有 Windows 字体路径后添加 Linux 路径：
     ```rust
     // Linux CJK 字体路径
     "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
     "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
     "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
     "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
     "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
     "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc",
     ```

2. **检查其他 Windows 特有代码**：
   - 审查 `app.rs`, `pages/*.rs` 中是否有路径硬编码
   - egui/eframe 本身跨平台，主要关注文件路径和进程调用

3. **验证**：
   ```bash
   cargo check -p ep-desktop  # 编译通过
   ```
   注意：服务器无 GUI 显示，不需要运行，只需编译通过。

**commit**：`fix(wave-1d): ep-desktop linux adaptation — CJK font paths`

---

### 8.5 Wave 2 — Agent E (PageDashboard)

**目标**：实现仪表盘页面。

**写入范围**：`frontend/src/pages/dashboard.tsx`, `frontend/src/components/shared/device-card.tsx`, `frontend/src/hooks/use-devices.ts`

**前置**：阅读 `docs/DESIGN_SYSTEM.md` 和 `frontend/src/api/types.ts`

**功能要求**：

1. **计算设备区域**：
   - 设备卡片网格（响应式：1-3 列）
   - 每张卡片：设备名称、后端类型徽章、显存进度条（颜色按 §5.2.4 规范）、利用率、温度
   - 无设备时显示空状态
   - 每 3 秒自动刷新（`use-devices.ts` hook）

2. **模块状态概览**：
   - 表格：名称、类别（中文标签）、状态徽章、设备、端口
   - 状态徽章使用 `status-badge.tsx` 共享组件（由 Agent I 提供，如未完成则自行创建局部版本）
   - 运行中模块数量摘要

3. **系统健康**：
   - 调用 `/api/deps` 显示 ffmpeg / torch CUDA 状态
   - 依赖缺失时显示警告卡片 + 安装引导

4. **依赖检测**：
   - 调用 `/api/deps` 显示 ffmpeg / torch CUDA 状态
   - 依赖缺失时显示警告卡片 + 安装引导

**UI 规范**：遵循 `DESIGN_SYSTEM.md` 的配色、排版、卡片样式。

---

### 8.6 Wave 2 — Agent F (PageModules)

**目标**：实现模块管理页面（最复杂的页面）。

**写入范围**：`frontend/src/pages/modules.tsx`, `frontend/src/pages/module-detail.tsx`, `frontend/src/components/shared/module-card.tsx`, `frontend/src/components/shared/log-viewer.tsx`, `frontend/src/hooks/use-modules.ts`, `frontend/src/hooks/use-module-detail.ts`

**功能要求**：

1. **模块列表** (`/modules`)：
   - 按 category 分组（使用 §5.2.6 中文标签）
   - 每组显示 "运行中/总数" 计数
   - 每个模块卡片：状态指示灯、名称、版本、类别、端口
   - 点击卡片进入详情
   - 顶部摘要栏：总数、运行中（绿）、已停止（灰）、错误（红）

2. **模块详情** (`/modules/:id`)：
   - 头部：状态徽章 + 模块名 + 版本
   - 信息网格：ID、版本、类别、状态、设备、端口、运行时长
   - 描述文本
   - 操作按钮：
     - Stopped → "启动" 按钮（绿色 outline）
     - Running/Starting → "停止" 按钮（红色 destructive）
     - Error → "重启" 按钮（先 stop 再 start）
     - NotReady → 警告提示
   - 操作中按钮显示 spinner + 禁用
   - 操作结果 Toast 通知

3. **日志查看器**：
   - 等宽字体、行号、深色背景
   - 自动滚动到底部（可锁定/解锁）
   - 最大高度 400px，内部滚动
   - 数据来源：REST `/api/modules/:id/logs`（历史）+ WebSocket `/ws/logs`（实时）
   - "清空" 按钮（仅清空前端显示）

4. **模型状态**：
   - 调用 `/api/models/:module_id` 显示模型列表
   - 每个模型：名称、状态（ready/missing）、大小
   - 本地导入表单（model_id + source_path → POST import）

---

### 8.7 Wave 2 — Agent G (PagePipeline)

**目标**：实现管线编辑器页面（React Flow DAG 可视化）。

**写入范围**：`frontend/src/pages/pipeline-editor.tsx`, `frontend/src/components/shared/pipeline-*.tsx`

**功能要求**：

1. **React Flow 画布**：
   - 自定义节点类型：
     - `ModuleNode`：显示模块名、capability、状态色
     - `BuiltinNode`：显示内置工具名（file_input/file_output/ffmpeg）
     - `ExternalApiNode`：显示 API 端点
   - 节点端口：输入（左）/ 输出（右），按 DataType 着色
   - 连线：贝塞尔曲线，带端口标签

2. **左侧节点面板**：
   - 从 `/api/modules` 获取可用模块及其 capabilities
   - 内置节点列表（file_input, file_output, ffmpeg）
   - 拖拽到画布创建节点

3. **右侧参数面板**：
   - 选中节点时显示
   - 根据 capability params schema 自动生成表单
   - 支持 string / boolean / integer / float 类型

4. **工具栏**：
   - 保存管线（序列化为 TOML 格式）
   - 加载管线（从文件读取 TOML）
   - 验证管线（调用 DAG 验证逻辑）
   - 执行管线（调用 `/api/pipelines/execute`，当前为占位）
   - 运行状态着色：灰=等待 / 蓝=运行 / 绿=完成 / 红=失败

5. **注意**：管线 API 目前是占位。前端先做好完整 UI，API 调用预留接口。执行功能在 API 实现后自动可用。

---

### 8.8 Wave 2 — Agent H (PageTaskSettings)

**目标**：实现任务中心、模型管理、设置三个页面。

**写入范围**：`frontend/src/pages/tasks.tsx`, `frontend/src/pages/models.tsx`, `frontend/src/pages/settings.tsx`, `frontend/src/hooks/use-config.ts`, `frontend/src/hooks/use-models.ts`

**任务中心** (`/tasks`)：
- 运行中服务列表（从 `/api/modules` 过滤 running/starting）
- 全部模块状态表格
- 管线任务列表（从 `/api/pipelines` 获取，当前为空）
- 任务详情：节点执行状态（从 `/api/pipelines/:id/status`，当前为占位）

**模型管理** (`/models`)：
- 按模块分组的模型列表（`/api/models`）
- 每个模型：名称、状态徽章（ready/missing/incomplete）、大小估计
- 展开显示详情：文件数、实际大小、本地缓存路径
- 本地导入表单：选择模块 → 选择模型 → 输入源路径 → 导入
- 导入进度/结果反馈

**设置** (`/settings`)：
- 表单对应 `/api/config` GET/PUT
- 分区：
  - **服务器**：监听地址（host）、端口（port）、「允许公网访问」开关（Switch）
    - 开启 `allow_public` 时弹出安全风险确认对话框（见 §4.5 警告文案）
    - 对话框确认后才写入配置
  - 通用：语言、主题、日志级别
  - 计算：分配策略（下拉）、允许超分（开关）、刷新间隔
  - 端口：模块端口范围起止（数字输入）
  - 模型：缓存目录、HF 镜像、默认来源
  - Python：python 路径、uv 路径（显示检测结果）
  - 管线：最大并行、默认超时、保留工作区
- 保存按钮（PUT /api/config）+ Toast 反馈
- 重置按钮（重新 GET）

---

### 8.9 Wave 2 — Agent I (SharedUX)

**目标**：实现共享 UX 基础设施。

**写入范围**：`frontend/src/components/layout/` (增强), `frontend/src/hooks/use-websocket.ts`, `frontend/src/hooks/use-toast.ts`, `frontend/src/stores/app-store.ts`, `frontend/src/lib/`, `frontend/src/components/shared/` (status-badge, confirm-dialog, empty-state, loading-skeleton)

**任务**：

1. **WebSocket 连接管理器** (`use-websocket.ts`)：
   - 自动重连（指数退避：1s → 2s → 4s → 8s → 最大 30s）
   - 连接状态暴露（connected / connecting / disconnected）
   - 心跳检测
   - JSON 消息解析
   - 页面不可见时暂停重连

2. **Toast 通知系统**：
   - 使用 shadcn/ui sonner 组件
   - 成功（绿）/ 错误（红）/ 警告（黄）/ 信息（蓝）
   - 右上角，自动消失 5s
   - 全局可调用（通过 hook 或 store）

3. **状态徽章** (`status-badge.tsx`)：
   - 圆点 + 文字
   - 颜色映射（§5.2.1 状态色）
   - Running 时圆点带 pulse 动画
   - 尺寸变体：sm / md

4. **确认对话框** (`confirm-dialog.tsx`)：
   - 标题 + 描述 + 确认/取消按钮
   - 破坏性操作变体（红色确认按钮）
   - Promise-based API：`const confirmed = await confirm({...})`

5. **空状态** (`empty-state.tsx`)：
   - 居中图标 + 标题 + 描述 + 可选操作按钮
   - 预设变体：no-modules, no-tasks, no-models, no-pipelines

6. **加载骨架屏** (`loading-skeleton.tsx`)：
   - 卡片骨架、表格骨架、列表骨架
   - 使用 shadcn/ui Skeleton 组件

7. **全局 Store** (`app-store.ts`)：
   - 主题状态（dark/light）
   - WebSocket 连接状态
   - 全局 loading 状态

8. **工具函数** (`lib/utils.ts`, `lib/constants.ts`)：
   - `cn()` 函数（clsx + tailwind-merge）
   - 格式化函数：`formatUptime(secs)`, `formatBytes(bytes)`, `formatMB(mb)`
   - 类别标签映射（§5.2.6）
   - 状态颜色映射

---

### 8.10 Wave 3 — Agent J (Integrator)

**目标**：构建前端、集成到 daemon、修复所有衔接问题。

**写入范围**：`crates/ep-daemon/src/main.rs` (SPA fallback), `frontend/` (构建修复), `crates/ep-core/src/process.rs` (日志流，如 Agent A 未完成)

**任务**：

1. **Daemon SPA Fallback**：
   - 修改 `main.rs` 的 fallback_service，使非 `/api/*`、非 `/ws/*` 路径返回 `index.html`
   - 使用 `ServeDir::not_found_service(ServeFile::new(...))`

2. **前端构建**：
   ```bash
   cd crates/ep-webui/frontend
   npm ci
   npm run build
   ```
   - 修复所有 TypeScript 编译错误
   - 修复所有组件导入/导出问题
   - 确保构建产物在 `../static/` 中

3. **API 类型对齐**：
   - 启动 daemon，curl 各端点
   - 对比实际响应与 `api/types.ts` 定义
   - 修复不匹配

4. **WebSocket 日志流**（如 Agent A 未完成）：
   - 修改 `process.rs`：将子进程 stdout/stderr 管道到 `log_tx`
   - 验证 `/ws/logs` 能收到模块日志

5. **Rust 构建**：
   ```bash
   cargo build --release
   cargo test
   cargo clippy
   ```

6. **端到端验证**：
   - 启动 daemon
   - 浏览器访问 `http://localhost:9800`
   - 验证所有页面可访问、API 调用正常

**commit**：`feat(wave-3j): integrate WebUI with daemon — SPA fallback, build, type alignment`

---

### 8.11 Wave 3 — Agent K (Tester)

**目标**：全面端到端测试。

**写入范围**：`reports/webui_test_report.md`

**任务**：

1. **API 测试**（curl）：
   - 所有 REST 端点返回正确状态码和数据格式
   - PUT /api/config 修改后 GET 验证
   - 模块 start/stop 流程

2. **页面测试**：
   - 所有 7 个路由可访问
   - 仪表盘显示设备信息
   - 模块列表正确分组
   - 设置页面可读写配置

3. **WebSocket 测试**：
   - 连接 `/ws/logs` 和 `/ws/progress`
   - 验证连接状态指示

4. **构建测试**：
   - `cargo test` 全部通过
   - `cargo clippy` 0 警告
   - `npm run build` 成功

5. **输出测试报告**：`reports/webui_test_report.md`

---

### 8.12 Wave 4 — Agent L (UXPolish)

**目标**：UI/UX 细节优化 + 双向同步到 egui 桌面端。

**写入范围**：`frontend/src/` (全局), `crates/ep-desktop/` (egui 同步), `docs/DESIGN_SYSTEM.md` (回写)

**任务**：

1. **WebUI 优化**：
   - 页面切换过渡动画
   - 模态框/抽屉动画
   - 空状态插图优化
   - 错误信息友好化（包含建议操作）
   - 键盘快捷键（`/` 聚焦搜索、`Esc` 关闭模态）
   - 响应式布局检查（移动端适配）
   - 整体视觉一致性审查

2. **回写 DESIGN_SYSTEM.md**：
   - 将新增的 UX 模式记录到设计规范
   - 更新组件行为规约

3. **egui 桌面端同步**：
   - 根据 DESIGN_SYSTEM.md 检查 egui 实现
   - 同步状态色、类别标签、布局模式
   - 确保 `cargo check -p ep-desktop` 通过

**commit**：`feat(wave-4l): UX polish + design system sync to egui`

---

### 8.13 Wave 4 — Agent M (Docs)

**目标**：更新所有项目文档。

**写入范围**：`README.md`, `PROGRESS.md`, `DESIGN.md`, `docs/`

**任务**：

1. **README.md**：
   - 添加 Linux 部署指南
   - systemd 安装步骤
   - 开发环境搭建（Rust + Node + uv）
   - WebUI 开发指南（dev server + proxy）

2. **PROGRESS.md**：
   - 添加 WebUI Wave 记录
   - 更新统计数据

3. **DESIGN.md**：
   - §2 技术栈添加 WebUI 部分
   - §5 UI 页面规划更新为 WebUI 实现
   - 添加 DESIGN_SYSTEM.md 引用

4. **docs/DEPLOYMENT.md**（新建）：
   - 完整部署步骤
   - systemd 配置说明
   - 防火墙配置（端口 9800）
   - 日志查看（journalctl）
   - 故障排查

**commit**：`docs(wave-4m): update README, PROGRESS, DESIGN + deployment guide`

---

## 9. API 参考

### 9.1 TypeScript 类型定义

Agent B 须在 `frontend/src/api/types.ts` 中定义以下类型（基于 daemon 实际响应）：

```typescript
// ─── Health ───
interface HealthResponse {
  status: string       // "ok"
  version: string      // "0.1.0"
}

// ─── Devices ───
interface DeviceResponse {
  id: string                // "cuda:0", "cpu"
  backend: string           // "cuda", "cpu", "rocm", "openvino", "directml"
  name: string              // "NVIDIA Tesla P4"
  total_memory_mb: number | null
  used_memory_mb: number | null
  utilization: number | null   // 0-100
  temperature: number | null   // Celsius
}

// ─── Modules ───
interface ModuleResponse {
  id: string
  name: string
  version: string
  description: string
  category: string
  path: string
  status: string            // "valid" | "invalid: <reason>"
  service_status: string    // "Running" | "Stopped" | "Starting" | ...
}

interface ModuleStatusResponse {
  module_id: string
  status: string    // "running" | "stopped" | "starting" | "preparing" | "not_ready" | "error"
  port: number | null
  uptime_secs: number
}

interface ModuleLogsResponse {
  module_id: string
  lines: string[]
}

interface ModuleActionResult {
  status?: string     // "starting" | "stopped"
  module_id?: string
  port?: number
  error?: string
}

// ─── Config ───
interface AppConfig {
  server: {
    host: string              // "0.0.0.0"
    port: number              // 9800
    allow_public: boolean     // false = 仅内网 IP 可访问
  }
  general: {
    language: string
    theme: string
    log_level: string
    check_updates: boolean
  }
  compute: {
    strategy: string
    disabled_backends: string[]
    refresh_interval_secs: number
    allow_overcommit: boolean
  }
  ports: {
    range_start: number
    range_end: number
  }
  models: {
    cache_dir: string
    hf_endpoint: string
    default_source: string
    max_concurrent_downloads: number
    cache_paths: string[]
  }
  python: {
    path: string
    uv_path: string
  }
  pipeline: {
    max_parallel: number
    default_timeout_secs: number
    keep_workspace: boolean
    workspace_dir: string
  }
  ui: {
    scale_factor: number
    font_size: number
    dashboard_refresh_secs: number
  }
}

// ─── Models ───
interface ModelListResponse {
  modules: {
    module_id: string
    module_name: string
    models: {
      model_id: string
      name: string
      target_dir: string
      status: string        // "ready" | "missing" | "incomplete" | "importable"
      source: string
      size_estimate_mb: number
    }[]
  }[]
}

interface ModelDetailResponse {
  module_id: string
  module_name: string
  models: {
    model_id: string
    name: string
    target_dir: string
    status: string
    size_bytes: number | null
    file_count: number | null
    local_cache_path: string | null
  }[]
}

interface ImportRequest {
  model_id: string
  source_path: string
}

interface ImportResponse {
  status?: string
  module_id?: string
  model_id?: string
  target_dir?: string
  file_count?: number
  total_bytes?: number
  error?: string
}

// ─── Dependencies ───
interface DepReport {
  ffmpeg: {
    available: boolean
    version: string | null
    path: string | null
    guidance: string | null
  }
  torch_cuda: {
    module_id: string
    available: boolean
    cuda_version: string | null
    guidance: string | null
  }[]
}

// ─── WebSocket Messages ───
interface WsLogMessage {
  module_id: string
  line: string
}

interface WsProgressMessage {
  pipeline_id: string
  node_id: string
  status: string
}
```

### 9.2 API Client 封装

```typescript
// api/client.ts
const BASE_URL = import.meta.env.DEV ? '' : ''  // 同源，无需 baseURL

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const resp = await fetch(`/api${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  })
  if (!resp.ok) {
    throw new Error(`API ${resp.status}: ${await resp.text()}`)
  }
  return resp.json()
}

// 使用示例
export const api = {
  health: () => apiFetch<HealthResponse>('/health'),
  devices: () => apiFetch<DeviceResponse[]>('/devices'),
  modules: () => apiFetch<ModuleResponse[]>('/modules'),
  moduleStatus: (id: string) => apiFetch<ModuleStatusResponse>(`/modules/${id}/status`),
  moduleLogs: (id: string) => apiFetch<ModuleLogsResponse>(`/modules/${id}/logs`),
  startModule: (id: string) => apiFetch<ModuleActionResult>(`/modules/${id}/start`, { method: 'POST' }),
  stopModule: (id: string) => apiFetch<ModuleActionResult>(`/modules/${id}/stop`, { method: 'POST' }),
  getConfig: () => apiFetch<AppConfig>('/config'),
  putConfig: (cfg: AppConfig) => apiFetch<AppConfig>('/config', { method: 'PUT', body: JSON.stringify(cfg) }),
  models: () => apiFetch<ModelListResponse>('/models'),
  moduleModels: (id: string) => apiFetch<ModelDetailResponse>(`/models/${id}`),
  importModel: (moduleId: string, req: ImportRequest) =>
    apiFetch<ImportResponse>(`/models/${moduleId}/import`, { method: 'POST', body: JSON.stringify(req) }),
  deps: () => apiFetch<DepReport>('/deps'),
}
```

---

## 10. 验证标准

### 10.1 每波次完成标准

| 波次 | 验证命令/标准 |
|---|---|
| Wave 0 | `rustc --version` ✅, `uv --version` ✅, `ffmpeg -version` ✅, `cargo check` ✅ |
| Wave 1 | `cargo test` 全通过, `cargo clippy` 0 警告, `npm run build` 成功, `docs/DESIGN_SYSTEM.md` 存在 |
| Wave 2 | `npm run build` 成功（TypeScript 0 错误），所有页面组件存在 |
| Wave 3 | `cargo build --release` ✅, daemon 启动后浏览器可访问所有页面, API 调用正常 |
| Wave 4 | `cargo test` ✅, `cargo clippy` ✅, `npm run build` ✅, 文档更新完成 |

### 10.2 最终验收标准

- [ ] `cargo test` 全部通过（Linux 上）
- [ ] `cargo clippy` 0 警告
- [ ] `cargo build --release` 成功
- [ ] `npm run build` 成功，产物在 `crates/ep-webui/static/`
- [ ] daemon 启动后，浏览器访问 `http://<host>:9800` 显示 WebUI
- [ ] 仪表盘显示 Tesla P4 GPU 信息
- [ ] 模块列表显示 5 个模块
- [ ] 设置页面可读写配置
- [ ] systemd 服务可安装、启动、开机自启
- [ ] `docs/DESIGN_SYSTEM.md` 完整
- [ ] 所有文档已更新

---

## 11. 风险与注意事项

### 11.1 技术风险

| 风险 | 影响 | 缓解措施 |
|---|---|---|
| RHEL 9 的 ffmpeg-free 功能受限 | 可能缺少某些编解码器 | 备选：johnvansickle.com 静态构建 |
| Python 3.9 不满足模块要求 | 模块无法运行 | uv 代管安装 3.12，本次不重建 venv |
| Tesla P4 (Pascal) CUDA 兼容性 | 某些新框架可能不支持 | CUDA 13.0 应兼容，注意 PyTorch 版本 |
| 前端构建产物体积 | 首屏加载慢 | Vite 代码分割 + 懒加载路由 |
| WebSocket 日志流未接通 | 实时日志不可用 | Agent A 或 Agent J 修复 process.rs |

### 11.2 协作注意事项

1. **文件冲突**：严格遵守 §7.3 写入隔离表。同一波次内两个代理不得写入同一文件。
2. **共享类型**：`api/types.ts` 由 Agent B 定义后冻结。Wave 2 代理如发现类型缺失，在自己的页面文件内定义局部类型，由 Agent J 统一合并。
3. **共享组件**：Wave 2 代理如需使用其他代理负责的共享组件（如 status-badge），先创建局部版本。Agent J 在 Wave 3 统一去重。
4. **Git 提交**：每个代理完成工作后自行 `git add` + `git commit`。commit message 格式：`<type>(wave-Nx/agent-y): <description>`。不 push。
5. **DESIGN_SYSTEM.md**：Wave 2 代理只读遵循，不得修改。修改权归 Agent B（创建）和 Agent L（回写）。

### 11.3 后续迭代（不在本次范围）

- Pipeline API 实现（daemon 接入 ep-core PipelineRunnerImpl）
- Python venv 重建（uv python install 3.12 + uv venv）
- 模型下载功能（HuggingFace / ModelScope）
- 国际化（中英双语）

> 注：本项目默认仅内网访问（代码层 IP 过滤），用户可手动开启公网但需自行保障安全。
> 用户认证 / HTTPS / 反向代理不内置，不在任何迭代计划中。

---

*文档结束。执行代理应以本文档为唯一工作参考。*
