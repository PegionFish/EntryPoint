# 夜间自治开发计划 — EntryPoint Phase 1→2 冲刺

> 创建时间: 2026-07-26 晚
> 目标: 天亮前完成 Phase 1 收尾 + Phase 2 核心，产出可运行的桌面应用原型
> 策略: 多代理并行 + 自循环验证

---

## 架构目标（qBittorrent 模式）

### 核心理念

EntryPoint 采用 **共享核心库 + 多入口** 架构，类似 qBittorrent：

```
ep-core (lib)              ← 共享核心库，所有功能在此
    │
    ├── ep-desktop (bin)   ← 直接 link ep-core
    │   (egui 原生 GUI)       无需 HTTP/IPC，零开销直调
    │   本地桌面使用
    │
    └── ep-daemon (bin)    ← 也 link ep-core
        (headless 服务)       额外暴露 HTTP API
        │                     用于 WebUI / 远程管理 / 服务器部署
        │
        └── 浏览器 (WebUI)  ← HTTP/WebSocket 连接 daemon
```

**桌面客户端不经过 HTTP**——它和 daemon 一样直接调用 `ep-core` 的 Rust 函数。
HTTP API 只存在于 daemon 中，专门为 WebUI 和远程访问服务。

### 关键约束

| 约束 | 说明 |
|---|---|
| **ep-core 是纯库** | 无 main、无二进制，只有 lib。所有业务逻辑在此 |
| **desktop 直连 core** | 桌面 GUI 直接 `use ep_core::*`，无网络层，无 IPC |
| **daemon 直连 core + 暴露 HTTP** | daemon = core 的 HTTP 包装层，给 WebUI/远程用 |
| **daemon 可独立部署** | 服务器上只跑 `ep-daemon`，无 GUI 依赖 |
| **desktop 不嵌入浏览器** | 纯 egui 原生渲染，不依赖 Electron/Tauri WebView |
| **WebUI 可选** | daemon 内置静态文件服务，可挂载 Web 前端（后续实现） |

### Workspace 目标结构

```
EntryPoint/
├── Cargo.toml                    # workspace root
├── crates/
│   ├── ep-core/                  # 核心逻辑库（不变）
│   │   └── src/
│   │       ├── types.rs
│   │       ├── config.rs
│   │       ├── module/
│   │       ├── compute/
│   │       ├── pipeline/
│   │       ├── process.rs
│   │       ├── health.rs
│   │       ├── env.rs
│   │       ├── model.rs
│   │       └── port.rs
│   │
│   ├── ep-daemon/                # 🆕 Headless 服务进程
│   │   └── src/
│   │       ├── main.rs           # tokio main，启动 HTTP server
│   │       ├── state.rs          # AppState: 持有所有 manager（直接用 ep-core）
│   │       ├── api/              # REST API 路由（给 WebUI/远程用）
│   │       │   ├── mod.rs
│   │       │   ├── modules.rs    # GET/POST /api/modules
│   │       │   ├── devices.rs    # GET /api/devices
│   │       │   ├── pipelines.rs  # POST /api/pipelines/execute
│   │       │   ├── config.rs     # GET/PUT /api/config
│   │       │   └── health.rs     # GET /api/health
│   │       └── ws/               # WebSocket 实时推送
│   │           ├── mod.rs
│   │           ├── logs.rs       # 模块日志流
│   │           └── progress.rs   # 管线进度流
│   │
│   ├── ep-desktop/               # 🔄 从 ep-ui 重命名
│   │   └── src/                  # 直接 use ep_core::*，无网络层
│   │       ├── main.rs           # eframe 启动
│   │       ├── app.rs            # AppState（直接持有 ep-core 的 manager）
│   │       └── pages/            # UI 页面
│   │
│   └── ep-webui/                 # 🆕 Web 前端（本轮仅建骨架）
│       └── static/               # 静态 HTML/JS（最简占位页，由 daemon 服务）
```

### 本轮范围（务实裁剪）

> 用户指示："确保后端架构设计完整可用即可，先不做大规模 UI 设计"

| 组件 | 本轮做 | 后续做 |
|---|---|---|
| ep-daemon (HTTP API) | ✅ 完整 REST API + WebSocket 骨架 | — |
| ep-desktop (egui) | ✅ 直连 ep-core + 基础页面 | 精细 UI 设计迭代 |
| ep-webui (浏览器) | ✅ 仅占位页面（证明 daemon 可服务 Web） | 完整 Vue/React 前端 |
| API 设计 | ✅ 完整覆盖所有核心功能（给 WebUI/远程用） | — |
| 远程管理 | ✅ daemon 监听 0.0.0.0，支持远程 WebUI 访问 | — |

### 技术约束备忘（搜索验证补充）

#### A. ep-desktop 的 tokio 异步集成

egui 运行在主线程，不能直接 `.await`。标准做法（来源：egui 社区 #521）：

```rust
// main.rs
fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();  // async → UI

    std::thread::spawn(move || {
        rt.block_on(async { /* spawn async tasks, tx.send() results */ });
    });

    eframe::run_native("EntryPoint", options, Box::new(move |cc| {
        Ok(Box::new(App::new(cc, rx)))
    }));
}

// app.rs — 每帧轮询 channel
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle_async_result(msg);
        }
    }
}
```

**约定**：async 任务通过 `tx.send()` + `ctx.request_repaint()` 通知 UI。

#### B. 优雅退出

- **daemon**: `tokio::signal::ctrl_c()` → 停止所有子进程 → 释放端口 → 退出
- **desktop**: 窗口关闭拦截 → 停止子进程 → 清理 → 退出

#### C. 端口隔离

daemon 和 desktop 同时运行时不能共享 PortManager 状态：
- 各自维护独立的 `PortManager` 实例
- 通过 lock file 或端口绑定检测避免冲突
- 默认端口范围相同，但先到先得

#### D. computer_use + eframe 共存（Wave 5）

1. `run_shell_command(is_background: true)` 启动 `entrypoint.exe`
2. 轮询 `computer_use__list_windows` 等待窗口出现
3. `computer_use__get_window_state` → `computer_use__click` 操作 UI
4. 测试完成后 `computer_use__kill_app` 关闭

---

## 0. 并行开发规则

### 0.1 文件写入隔离（铁律）

每个 agent 只能写入自己名下的文件。违反 = 冲突 = 灾难。

| 文件 | 只读/写入 | W1a-A | W1a-B | W1b-C | W2-D | W2-E | W2-D2 | W4-F/G | W5 | W3/6 |
|---|---|---|---|---|---|---|---|---|---|---|
| `types.rs` | READ-ONLY | — | — | — | — | — | — | — | — | — |
| `lib.rs` | Wave 0/3 | — | — | — | — | — | — | — | — | ✏️ |
| `process.rs` | W1a-A | ✏️ | — | — | — | — | — | — | — | — |
| `health.rs` (新) | W1a-A | ✏️ | — | — | — | — | — | — | — | — |
| `compute/scheduler.rs` (新) | W1a-B | — | ✏️ | — | — | — | — | — | — | — |
| `compute/mod.rs` | W1a-B | — | ✏️ | — | — | — | — | — | — | — |
| `pipeline/executor.rs` | W1b-C | — | — | ✏️ | — | — | — | — | — | — |
| `pipeline/runner.rs` (新) | W1b-C | — | — | ✏️ | — | — | — | — | — | — |
| `pipeline/mod.rs` | W1b-C | — | — | ✏️ | — | — | — | — | — | — |
| `ep-desktop/main.rs` | W2-D | — | — | — | ✏️ | — | — | — | — | — |
| `ep-desktop/app.rs` | W2-D | — | — | — | ✏️ | — | — | — | — | — |
| `ep-desktop/pages/*.rs` | W2-D | — | — | — | ✏️ | — | — | — | — | — |
| `module/lifecycle.rs` (新) | W2-E | — | — | — | — | ✏️ | — | — | — | — |
| `env.rs` | W2-E | — | — | — | — | ✏️ | — | — | — | — |
| `model.rs` | W2-E | — | — | — | — | ✏️ | — | — | — | — |
| `ep-daemon/src/api/*.rs` | W2-D2 | — | — | — | — | — | ✏️ | — | — | — |
| `ep-daemon/src/ws/*.rs` | W2-D2 | — | — | — | — | — | ✏️ | — | — | — |
| `ep-daemon/src/state.rs` | W2-D2 | — | — | — | — | — | ✏️ | — | — | — |
| `ep-daemon/src/main.rs` | W2-D2 | — | — | — | — | — | ✏️ | — | — | — |
| `tests/integration_*.rs` (新) | W4-F | — | — | — | — | — | — | ✏️ | — | — |
| `config/` (新) | W4-G/W5 | — | — | — | — | — | — | ✏️ | ✏️ | — |
| `reports/e2e_test_report.md` | W5 | — | — | — | — | — | — | — | ✏️ | — |
| `Cargo.toml` (各) | Wave -1/0/3/6 | — | — | — | — | — | — | — | — | ✏️ |

### 0.2 接口契约（Wave 0 锁定，后续只读）

Wave 0 在 `types.rs` 中定义以下 trait，所有 agent 面向 trait 编程：

```rust
// ── ProcessManager 契约 ──
pub trait ModuleProcess: Send + Sync {
    fn start(&mut self, cfg: &StartConfig) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn status(&self) -> &ServiceStatus;
    fn logs(&self) -> &VecDeque<String>;
    fn pid(&self) -> Option<u32>;
}

// ── ComputeScheduler 契约 ──
pub trait DeviceScheduler: Send + Sync {
    fn assign(&self, module_id: &str, backends: &[ComputeBackend], vram_mb: u32) -> Option<DeviceId>;
    fn release(&mut self, module_id: &str);
    fn devices(&self) -> &[ComputeDevice];
}

// ── PipelineRunner 契约 ──
pub trait PipelineRunner: Send + Sync {
    fn execute(&mut self, pipeline: &Pipeline, work_dir: &Path) -> Result<()>;
    fn task_status(&self) -> &TaskStatus;
    fn node_status(&self, node_id: &str) -> Option<&NodeState>;
}
```

### 0.3 Agent 指令模板

每个 agent 收到的 prompt 必须包含：
1. **目标**：实现什么，验收标准
2. **写入范围**：只能写哪些文件
3. **只读依赖**：需要读哪些文件了解接口
4. **约束**：不能做什么
5. **验证**：写完后自行 cargo check 确认编译通过

### 0.4 验证循环

每波结束后，主代理执行：
```
1. cargo check → 编译通过？
   ├─ YES → cargo test → 全部通过？
   │         ├─ YES → 进入下一波
   │         └─ NO  → 定位失败测试 → 修复 → 重测
   └─ NO  → 读错误 → 修复 → 重检
2. 记录进度到 PROGRESS.md
3. 启动下一波 agent 或进入下一循环
```

### 0.5 Git 提交规则（灾备铁律）

> 每个 Agent 完成任务后必须自行 commit，确保任何时刻都能从最近的 commit 快速恢复。

#### 提交时机

| 角色 | 何时 commit | commit message 格式 |
|---|---|---|
| 主代理 (Wave 0/3/5) | 每波完成后 | `chore(wave-N): 描述` |
| Agent A-G | 任务完成后 | `feat(wave-N/agent-X): 描述` |
| 主代理 (修复) | 每次修复后 | `fix(wave-N): 修复描述` |

#### 提交流程（每个 Agent 必须执行）

```
1. git add <你写入的所有文件>
   — 只 add 你自己写的文件，不要 git add -A
2. git diff --staged --stat
   — 确认只包含你的改动
3. git commit -m "feat(wave-N/agent-X): 简要描述"
   — message 必须标明 wave 和 agent 编号
4. 确认 commit 成功（git log -1 验证）
```

#### 恢复策略

```
灾难恢复：
  git log --oneline -10          # 找到最后一个好的 commit
  git diff HEAD <bad-commit>     # 看看坏了什么
  git reset --hard <good-commit> # 回滚到好的点
  # 然后主代理重新执行失败的 wave
```

#### 并行 Agent 的提交注意

- Wave 1a 的 Agent A 和 Agent B 并行完成后，各自 commit 自己的文件
- 主代理在检查点先 `git pull` 式的检查（`git log` 看两个 commit 都在）
- 如果两个 agent 的 commit 有冲突（理论上不应发生，因为文件隔离），主代理手动合并

---

## 1. 工作波次（修订版）

### Wave -1：架构拆分（主代理，串行）⚡ 最高优先级

**目标**：将当前 2-crate workspace 拆分为 4-crate qBittorrent 架构

**步骤**：

1. **重命名 ep-ui → ep-desktop**
   - `git mv crates/ep-ui crates/ep-desktop`
   - 更新 `crates/ep-desktop/Cargo.toml` 中 `package.name = "ep-desktop"`
   - 更新 workspace `Cargo.toml` 的 members 和 dependencies

2. **创建 ep-daemon crate**
   - `crates/ep-daemon/Cargo.toml` — 依赖: ep-core, axum, tokio, serde, tower-http
   - `crates/ep-daemon/src/main.rs` — tokio main，启动 axum HTTP server
   - `crates/ep-daemon/src/state.rs` — `AppState`（Arc 包裹所有 manager）
   - `crates/ep-daemon/src/api/mod.rs` — 路由注册
   - `crates/ep-daemon/src/api/health.rs` — `GET /api/health`（最简端点，验证架构可用）
   - `crates/ep-daemon/src/ws/mod.rs` — WebSocket 骨架

3. **创建 ep-webui crate（仅骨架）**
   - `crates/ep-webui/` — 空 crate，仅含 `static/index.html` 占位页
   - daemon 的 `main.rs` 中加 `tower-http::services::ServeDir` 挂载此目录

4. **更新 workspace Cargo.toml**
   ```toml
   [workspace]
   members = [
       "crates/ep-core",
       "crates/ep-daemon",
       "crates/ep-desktop",
       "crates/ep-webui",
   ]

   [workspace.dependencies]
   axum = { version = "0.8", features = ["ws"] }
   tower-http = { version = "0.6", features = ["cors", "fs"] }
   ep-core = { path = "crates/ep-core" }
   ep-daemon = { path = "crates/ep-daemon" }
   ```

5. **验证**
   - `cargo check` — 全 workspace 编译通过
   - `cargo test` — 原有 77 个测试全部通过（ep-core 不变）
   - `cargo run -p ep-daemon` — 启动 daemon，`curl localhost:9800/api/health` 返回 `{"status":"ok"}`
   - `cargo run -p ep-desktop` — 桌面程序仍能启动（直连 ep-core，无网络依赖）

6. **commit**: `refactor(wave--1): split workspace into daemon + desktop + webui architecture`

**预计耗时**: 15-20 分钟

---

### Wave 0：基础准备（主代理，串行）

**目标**：定义共享 trait + 新建空文件骨架 + 确保编译通过

- [ ] 在 `types.rs` 添加 `ModuleProcess`、`DeviceScheduler`、`PipelineRunner` trait 定义
- [ ] 创建空文件（仅含 struct 骨架 + trait impl 占位）：
  - `compute/scheduler.rs`
  - `module/lifecycle.rs`
  - `health.rs`
  - `pipeline/runner.rs`
- [ ] 在 `lib.rs` 中声明新模块（`pub mod health;` 等）
- [ ] `cargo check` 确认通过
- [ ] 写 `PROGRESS.md` 初始状态

**预计耗时**: 10-15 分钟

---

### Wave 1a：进程管理 + 设备调度（2 agent 并行）

> 这两个模块互不依赖，可以安全并行。Wave 1b 的管线执行依赖它们定义的 trait。

#### Agent A — "ProcessForge"
**任务**: 实现真正的进程管理 + 健康检查

**写入文件**:
- `crates/ep-core/src/process.rs` — 重写，加入 tokio::process::Child
- `crates/ep-core/src/health.rs` — 新建，HTTP 健康检查

**具体要求**:
1. `ProcessManager` 持有 `HashMap<String, ServiceInstance>`
2. `ServiceInstance` 包含 `Option<tokio::process::Child>` 实际子进程句柄
3. `start_module()` 实际 spawn 子进程：
   - 构建 Command（program + args from start_command template）
   - 设置环境变量（EP_PORT, EP_DEVICE, EP_MODEL_DIR 等）
   - 捕获 stdout/stderr 到 log_buffer
   - 存储 Child handle
4. `stop_module()` 实际 kill 子进程（`child.kill().await`）
5. `check_health()` — 轮询模块的 `/health` 端点直到成功或超时
6. `monitor_process()` — 检查子进程是否意外退出，更新状态为 Error
7. 所有异步方法使用 `async fn` + tokio

**测试要求**: 至少 5 个测试（可用 mock 命令如 `cmd /c echo` 作为假模块）

---

#### Agent B — "DeviceMaster"
**任务**: 实现计算设备调度器

**写入文件**:
- `crates/ep-core/src/compute/scheduler.rs` — 新建
- `crates/ep-core/src/compute/mod.rs` — 添加 `pub mod scheduler;` 和 re-export

**具体要求**:
1. `ComputeScheduler` 结构体：
   - `devices: Vec<ComputeDevice>` — 已知设备列表
   - `assignments: HashMap<String, DeviceId>` — module_id → device 映射
2. 实现四种分配策略：
   - `Manual` — 用户指定，调度器只验证兼容性
   - `LeastMemory` — 选剩余显存最大的兼容设备
   - `RoundRobin` — 轮询分配
   - `Single(DeviceId)` — 全部用一个设备
3. `assign()` 检查：
   - 模块声明的 backends 是否包含目标设备的 backend
   - 预估显存 vs 剩余显存（超限时 warning 但不阻止，如果 allow_overcommit=true）
4. `release()` 释放设备分配
5. `status_report()` 返回所有设备的分配摘要

**测试要求**: 至少 8 个测试（覆盖四种策略、兼容性检查、显存超限）

---

**⏸ Wave 1a 检查点** — 主代理验证编译 + 测试通过后启动 Wave 1b

---

### Wave 1b：管线执行引擎（1 agent）

> 依赖 Wave 1a 的 ProcessManager trait 和 ComputeScheduler trait

#### Agent C — "PipelineCrusher"
**任务**: 实现管线实际执行引擎

**写入文件**:
- `crates/ep-core/src/pipeline/executor.rs` — 增强，加入实际执行逻辑
- `crates/ep-core/src/pipeline/runner.rs` — 新建，高层执行接口
- `crates/ep-core/src/pipeline/mod.rs` — 添加 `pub mod runner;`

**具体要求**:
1. `PipelineRunner` 结构体：
   - 持有 `PipelineTask`（已有状态机）
   - 持有 `reqwest::Client`（HTTP 调用模块 API）
2. `execute_layer()` 实际执行：
   - `NodeKind::Module` → `reqwest::post("http://localhost:{port}/predict/{capability}")` + multipart 文件上传
   - `NodeKind::Builtin::FFmpeg` → `tokio::process::Command::new("ffmpeg")` + 参数
   - `NodeKind::Builtin::FileInput/FileOutput` → 文件复制/移动
   - `NodeKind::ExternalApi` → `reqwest::post(endpoint)` + JSON body
3. 同层节点 `tokio::spawn` 并行执行
4. 节点间数据传递：上游输出文件路径 → 下游输入参数
5. 错误处理：节点失败 → 下游全部 Skipped
6. 进度回调：`on_node_start`、`on_node_complete`、`on_node_error`

**测试要求**: 至少 5 个测试（用纯 builtin 节点测试，不依赖外部服务）

---

**⏸ Wave 1b 检查点** — 主代理验证编译 + 测试，修复跨模块引用

---

### Wave 2：集成层（3 agent 并行）

#### Agent D — "UIWeaver"
**任务**: 全量重写 ep-desktop UI，直接集成 ep-core 管理器（无网络层）

**关键架构约束**：ep-desktop 必须用独立线程跑 tokio runtime（详见"技术约束备忘 A"）：
- `main.rs` 在独立线程创建 `tokio::runtime::Runtime`
- `mpsc::channel` 连接 async 任务 → UI
- `app.rs` 每帧 `try_recv()` 轮询 + `ctx.request_repaint()` 唤醒

**写入文件**（注意：Wave -1 已将 ep-ui 重命名为 ep-desktop）:
- `crates/ep-desktop/src/main.rs` — 重写：独立 tokio 线程 + eframe 主线程
- `crates/ep-desktop/src/app.rs` — 重写 AppState，直接持有 ep-core 的 manager + mpsc::Receiver
- `crates/ep-desktop/src/pages/dashboard.rs` — 显示真实设备/模块状态
- `crates/ep-desktop/src/pages/modules.rs` — 真实启动/停止/日志
- `crates/ep-desktop/src/pages/settings.rs` — 真实配置保存/加载
- `crates/ep-desktop/src/pages/tasks.rs` — 显示真实任务状态
- `crates/ep-desktop/src/pages/pipeline_editor.rs` — 基础管线加载/验证

**具体要求**:
1. `AppState` 持有：
   - `AppConfig` — 从 `config/app.toml` 加载
   - `Vec<ComputeDevice>` — 来自 `detect_all_devices()`
   - `Vec<DiscoveredModule>` — 来自 `discover_modules()`
   - `ProcessManager` — 管理运行中的模块
   - `PortManager` — 端口分配
2. Dashboard 自动刷新（每 2s 通过 `ctx.request_repaint()` 触发）
3. 模块页面：
   - "启动" 按钮 → `port_manager.allocate()` → `process_manager.start_module()`
   - "停止" 按钮 → `process_manager.stop_module()` → `port_manager.release()`
   - 日志面板 → `process_manager.get_instance().log_buffer`
4. 设置页面：修改后点"保存" → `config.save()`
5. 启动时自动检测：设备 + 模块 + Python/uv 状态

**验证**: 编译通过 + 手动验证 UI 能启动

---

#### Agent E — "EnvRunner"
**任务**: 完善环境管理 + 模块生命周期

**写入文件**:
- `crates/ep-core/src/module/lifecycle.rs` — 新建
- `crates/ep-core/src/env.rs` — 增强（添加 venv 状态查询）
- `crates/ep-core/src/model.rs` — 增强（添加下载执行）

**具体要求**:
1. `ModuleLifecycle` 结构体 — 编排模块的完整生命周期：
   ```
   discover → validate → check_env → setup_env → check_model → download_model → ready → start → run → stop
   ```
2. 整合 `EnvManager` + `ModelManager` + `ProcessManager` 的调用
3. `EnvManager` 增强：
   - `check_all_modules_env()` — 批量检查所有已发现模块的环境状态
   - `get_venv_status(module_id)` — 返回 venv 状态枚举（NotExist / Ready / NeedsUpdate）
4. `ModelManager` 增强：
   - `execute_download()` — 实际调用 `build_download_command()` 返回的命令并执行
   - 解析 stdout 输出提取下载进度百分比
   - 支持取消下载（kill 下载进程）
5. `get_module_readiness()` — 返回模块就绪状态（缺环境/缺模型/就绪/运行中）

**测试要求**: 至少 6 个测试

---

#### Agent D2 — "DaemonForge"
**任务**: 实现 ep-daemon 的完整 HTTP REST API + WebSocket 骨架

**写入文件**:
- `crates/ep-daemon/src/state.rs` — AppState（Arc 包裹所有 ep-core manager）
- `crates/ep-daemon/src/api/mod.rs` — 路由注册
- `crates/ep-daemon/src/api/modules.rs` — 模块管理 API
- `crates/ep-daemon/src/api/devices.rs` — 设备查询 API
- `crates/ep-daemon/src/api/pipelines.rs` — 管线执行 API
- `crates/ep-daemon/src/api/config.rs` — 配置读写 API
- `crates/ep-daemon/src/api/health.rs` — 健康检查
- `crates/ep-daemon/src/ws/mod.rs` — WebSocket 路由
- `crates/ep-daemon/src/ws/logs.rs` — 日志流
- `crates/ep-daemon/src/ws/progress.rs` — 进度流
- `crates/ep-daemon/src/main.rs` — 增强：完整启动流程

**API 设计**:
```
GET    /api/health              → {"status":"ok","version":"0.1.0"}
GET    /api/devices             → 计算设备列表（实时检测）
GET    /api/modules             → 已发现模块列表 + 状态
POST   /api/modules/:id/start   → 启动模块
POST   /api/modules/:id/stop    → 停止模块
GET    /api/modules/:id/logs    → 获取日志
GET    /api/config              → 当前配置
PUT    /api/config              → 更新配置
GET    /api/pipelines           → 管线列表
POST   /api/pipelines/execute   → 执行管线
GET    /api/pipelines/:id/status → 管线执行状态

WS     /ws/logs                 → 实时日志流
WS     /ws/progress             → 管线进度推送送
```

**具体要求**:
1. `AppState` 用 `Arc<tokio::sync::RwLock<...>>` 包裹 ep-core 的 manager
2. daemon 监听 `0.0.0.0:9800`（端口可配置）
3. 所有 API 返回 JSON
4. CORS 支持（允许 WebUI 跨域访问）
5. WebSocket 用 `tokio::sync::broadcast` 广播日志/进度
6. 静态文件服务：挂载 `ep-webui/static/` 目录

**测试要求**: 至少 5 个测试（用 `axum::test` 的 TestClient）

---

**⏸ Wave 2 检查点** — 主代理验证编译 + 测试，修复跨模块引用

---

### Wave 3：集成 + 冒烟（主代理，串行）

- [ ] 合并所有 agent 的产出
- [ ] 更新 `lib.rs` 声明所有新模块
- [ ] 更新 `Cargo.toml` 添加新依赖（如有）
- [ ] `cargo check` 全量编译
- [ ] 修复跨模块引用错误
- [ ] `cargo test` 全量测试
- [ ] `cargo clippy` 检查
- [ ] 修复所有 warning
- [ ] 尝试 `cargo build --release`
- [ ] 更新 `PROGRESS.md`

---

### Wave 4：集成测试（1-2 agent 并行）

#### Agent F — "TestHawk"
**任务**: 编写端到端集成测试

**写入文件**:
- `crates/ep-core/tests/integration_module_lifecycle.rs` — 模块发现→环境检查→启动→停止 全流程
- `crates/ep-core/tests/integration_pipeline.rs` — 加载管线 TOML → 执行 builtin 节点 → 验证输出
- `crates/ep-core/tests/integration_compute.rs` — 设备检测→调度分配→释放 全流程

**具体要求**:
1. 模块生命周期测试：
   - 创建临时 modules/ 目录 + 写入有效 module.toml
   - 发现模块 → 验证 manifest → 检查环境状态
   - 用 mock 命令（`cmd /c echo`）模拟模块启动/停止
2. 管线执行测试：
   - 编写测试用管线 TOML（FileInput → FFmpeg → FileOutput）
   - 执行管线 → 验证节点状态转换 → 验证输出文件存在
3. 设备调度测试：
   - 构造假设备列表 → 测试各策略分配结果
   - 测试兼容性过滤、显存超限警告

**测试要求**: 至少 8 个集成测试

---

#### Agent G — "PolishCat"（可选，视时间）
**任务**: clippy 清理 + 文档补全 + 示例配置

**写入文件**:
- `config/app.toml` — 默认配置模板文件
- `config/pipelines/video_to_srt.toml` — 示例管线
- 各模块的 clippy 修复

**具体要求**:
1. 运行 `cargo clippy -- -D warnings` 并修复所有问题
2. 创建 `config/` 目录下的默认配置文件和示例管线
3. 确保 `cargo doc --no-deps` 无警告

---

**⏸ Wave 4 检查点** — 最终验证

---

### Wave 5：实机端到端测试（主代理 + computer_use）

> 测试机: RTX 5090 D (32GB VRAM)
> 测试素材: `D:\AI_Applications\[Banngai] Shoushimin Series 2nd Season [01][WEB-DL][1080P_AVC_AAC].mkv`

#### 5.1 部署准备

- [ ] `cargo build --release` 编译 release 二进制
- [ ] 确保 `D:\AI_Applications\EntryPoint\` 目录结构完整：
  ```
  EntryPoint/
  ├── target/release/entrypoint.exe  ← 主程序
  ├── config/app.toml                ← 默认配置
  ├── modules/                       ← 模块目录
  └── ...
  ```
- [ ] 创建 `modules/` 下的示例模块目录（至少一个可运行的模块，如 faster-whisper 的 module.toml）
- [ ] commit: `chore(wave-5): deploy build for e2e testing`

#### 5.2 启动应用 + 基础验证

使用 `computer_use` 工具操作 GUI：

1. **启动应用**
   - `run_shell_command` 启动 `target/release/entrypoint.exe`（后台）
   - `computer_use__start_session` 声明测试 session
   - `computer_use__get_window_state` 获取窗口状态，确认 UI 已加载

2. **仪表盘验证**
   - 截图确认仪表盘显示
   - 验证 RTX 5090 D 被正确检测（应显示 ~32GB VRAM、CUDA backend）
   - 验证 CPU 设备也被检测

3. **模块页面验证**
   - 点击左侧导航 "🧩 模块"
   - 截图确认模块列表显示
   - 如果有 module.toml，验证模块被发现

4. **设置页面验证**
   - 点击 "⚙ 设置"
   - 验证配置项可编辑
   - 修改一个设置 → 保存 → 重新加载验证持久化

#### 5.3 视频管线端到端测试

核心测试：用 MKV 文件走一遍完整的视频处理管线

1. **准备测试管线 TOML**
   - 创建 `config/pipelines/test_video_to_audio.toml`
   - 最简管线: `FileInput → FFmpeg(提取音频) → FileOutput`
   - 如果模块已就绪，扩展为: `FileInput → FFmpeg → Denoise → ASR → FileOutput`

2. **通过 UI 或 CLI 触发管线执行**
   - 如果 UI 有管线执行入口 → 通过 computer_use 操作
   - 否则 → 通过 `run_shell_command` 直接调用

3. **验证输出**
   - 检查 `workspace/` 目录下是否生成了输出文件
   - 如果走了 ASR → 检查输出文本是否包含识别结果
   - 截图记录最终状态

#### 5.4 GPU 专项验证

- [ ] 确认 `nvidia-smi` 输出与 UI 仪表盘显示一致
- [ ] 如果启动了模型服务，验证 VRAM 占用合理
- [ ] 确认 CUDA backend 分配正确（`cuda:0` → RTX 5090 D）

#### 5.5 测试报告

截图 + 文字记录：
```markdown
## 实机测试报告
### 环境
- GPU: NVIDIA RTX 5090 D (32GB)
- OS: Windows
- 测试文件: [Banngai] Shoushimin Series 2nd Season [01].mkv

### 测试结果
| 测试项 | 状态 | 截图 | 备注 |
|---|---|---|---|
| 应用启动 | ✅/❌ | screenshot_01.png | ... |
| GPU 检测 | ✅/❌ | screenshot_02.png | ... |
| 模块发现 | ✅/❌ | screenshot_03.png | ... |
| 设置保存 | ✅/❌ | screenshot_04.png | ... |
| 管线执行 | ✅/❌ | screenshot_05.png | ... |
| 输出文件 | ✅/❌ | — | 文件路径: ... |
```

- [ ] commit: `test(wave-5): e2e test results + screenshots`

---

### Wave 6：最终报告（主代理）

- [ ] `cargo check` ✅
- [ ] `cargo test` — 全部通过
- [ ] `cargo clippy` — 无警告
- [ ] `cargo build --release` — 成功
- [ ] 实机测试通过
- [ ] 统计：总测试数、总代码行数、模块数
- [ ] 写最终 `PROGRESS.md`
- [ ] 结束循环

---

## 2. 循环调度机制

### 2.1 主循环时间线

```
22:00  主代理启动
  │
22:15  ├─ Wave -1: 架构拆分（ep-ui→ep-desktop + 新建 ep-daemon/ep-webui）✓
  │    └─ git commit: "refactor(wave--1): ..."
  │
22:25  ├─ Wave 0: 基础准备（trait 定义 + 骨架）✓
  │    └─ git commit: "chore(wave-0): ..."
  │
22:30  ├─ 启动 Wave 1a: 2 agent 并行
  │    ├─ Agent A (ProcessForge)
  │    └─ Agent B (DeviceMaster)
  │
22:50  ├─ ⏸ loop_wakeup(1200s) — 20min 后检查
  │
23:10  │  [wakeup] 检查 Wave 1a → cargo check + test → ✓
  │
23:15  ├─ 启动 Wave 1b: 1 agent
  │    └─ Agent C (PipelineCrusher)
  │
23:35  ├─ ⏸ loop_wakeup(1200s)
  │
23:55  │  [wakeup] 检查 Wave 1b → cargo check + test → ✓
  │
00:00  ├─ 启动 Wave 2: 3 agent 并行
  │    ├─ Agent D  (UIWeaver)     — ep-desktop UI 重写
  │    ├─ Agent E  (EnvRunner)    — 环境管理 + 生命周期
  │    └─ Agent D2 (DaemonForge)  — ep-daemon HTTP API + WebSocket
  │
00:25  ├─ ⏸ loop_wakeup(1200s)
  │
00:45  │  [wakeup] 检查 Wave 2 → cargo check + test → ✓
  │
00:50  ├─ Wave 3: 集成 + 冒烟（主代理串行）
  │    ├─ 验证: cargo run -p ep-daemon → curl /api/health ✓
  │    ├─ 验证: cargo run -p ep-desktop → UI 启动 ✓
  │    └─ git commit: "chore(wave-3): integration"
  │
01:10  ├─ 启动 Wave 4: 集成测试 agent
  │    ├─ Agent F (TestHawk)
  │    └─ Agent G (PolishCat) [可选]
  │
01:30  ├─ ⏸ loop_wakeup(1200s)
  │
01:50  │  [wakeup] 检查 Wave 4 → ✓
  │
01:55  ├─ Wave 5: 实机端到端测试（computer_use）
  │    ├─ cargo build --release
  │    ├─ 启动 ep-daemon → curl 验证 API
  │    ├─ 启动 ep-desktop → computer_use 操作 GUI
  │    ├─ 验证 RTX 5090 D 检测
  │    ├─ 用 MKV 跑视频管线
  │    └─ 截图 + 测试报告 → commit
  │
02:25  ├─ Wave 6: 最终报告
  │
02:35  └─ 结束循环
```

**总预计耗时**: ~4.5 小时（含架构拆分 + 实机测试缓冲）

### 2.2 错误恢复策略

| 情况 | 处理 |
|---|---|
| Agent 编译失败 | 主代理读错误信息 → 直接修复 → commit `fix(...)` → 重检 |
| Agent 测试失败 | 主代理分析失败原因 → 修复 → commit → 或标记跳过 → 记录到 PROGRESS.md |
| Agent 超时（>25min 无响应） | 发送补充指令；再 5min 无响应则终止并主代理手动完成 |
| Agent commit 失败 | 主代理代为 commit（用 agent 的文件列表） |
| 跨 agent 接口冲突 | 主代理统一调整 trait 接口 → 修复双方文件 → commit `fix(wave-N): resolve interface conflict` |
| 连续 2 次修复仍失败 | 跳过该任务，记录原因，继续下一波 |
| 整个 Wave 失败 | 记录到 PROGRESS.md，跳到下一个 Wave（不阻塞整体进度） |
| **灾难性错误**（代码库损坏） | `git log --oneline -10` 找最后好的 commit → `git reset --hard` → 从该点重新执行 |

### 2.3 进度追踪

每波结束后更新 `PROGRESS.md`：
```markdown
## Wave N — ✅ 完成 / ❌ 部分失败 — HH:MM
### 完成
- [x] 具体完成项
### 失败/跳过
- [ ] 未完成项（原因）
### 编译状态
- cargo check: ✅/❌
- cargo test: X/Y passed
### Git
- 最新 commit: `abc1234` — "feat(wave-N/agent-X): 描述"
- 上一稳定点: `def5678` — "chore(wave-M): 描述"
### 下一步
- ...
```

---

## 3. Agent 指令模板

每个 background agent 的 prompt 遵循以下结构：

```
你是 EntryPoint 项目的并行开发 agent。

## 你的任务
{具体目标描述}

## 写入范围（铁律：只能写这些文件）
- {file1}
- {file2}

## 只读依赖（可以读这些文件了解接口）
- {dep1}
- {dep2}

## 禁止
- 不得修改写入范围之外的任何文件
- 不得修改 types.rs（它是共享契约，已锁定）
- 不得修改 Cargo.toml（依赖由主代理统一管理）

## 验收标准
1. cargo check 通过（用 C:\Users\PegionFish\.cargo\bin\cargo.exe check）
2. 你的模块的测试全部通过
3. 至少 {N} 个有意义的单元测试

## 完成后必须执行
1. cargo check — 确认编译通过
2. cargo test — 确认测试通过
3. git add <你写入的所有文件>（不要 git add -A）
4. git commit -m "feat(wave-N/agent-X): 简要描述"
5. git log -1 确认 commit 成功

## 最终输出
在最后一个 tool output 中总结：
- 实现了什么
- 测试数量和覆盖范围
- commit hash
- 已知限制/TODO
```

---

## 4. 预期产出

天亮时应有：

1. ✅ **可编译的完整项目** — `cargo build --release` 通过
2. ✅ **真实进程管理** — 能启动/停止 Python adapter 子进程
3. ✅ **设备调度** — 自动检测 GPU 并按策略分配（RTX 5090 D 已验证）
4. ✅ **管线执行** — 能加载 TOML 管线定义并逐节点执行
5. ✅ **连接的 UI** — 仪表盘显示真实设备、模块可启动/停止
6. ✅ **环境管理** — 自动检测 Python/uv、创建 venv
7. ✅ **集成测试** — 端到端验证模块生命周期、管线执行、设备调度
8. ✅ **120+ 单元+集成测试** — 覆盖所有核心模块
9. ✅ **完整文档** — MODULE_SPEC, ADAPTER_API, CONFIG_REFERENCE, PIPELINE_SPEC
10. ✅ **示例配置** — 默认 app.toml + 示例管线 video_to_srt.toml
11. ✅ **实机测试报告** — 含截图，证明 UI 可启动、GPU 已检测、管线可执行
12. ✅ **完整 git 历史** — 每波都有 commit，可随时回滚
