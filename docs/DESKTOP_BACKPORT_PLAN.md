# EntryPoint 桌面 GUI 反向移植 + 架构优化 + Arch 打包 — 执行方案

> ⚠️ **Sunset 横幅（2026-08-13）**：本文档所述 **ep-desktop 桌面端已于 2026-08-13 退役**，WebUI 为唯一 UI（server 形态交付）。本页保留为历史记录，不再维护；详见 [DESKTOP_SUNSET_PLAN.md](DESKTOP_SUNSET_PLAN.md)。

> 版本：v4 (最终) | 日期：2026-07-30
>
> 状态：待确认

---

## 1. 背景与目标

WebUI（React，56 个 ts/tsx 文件，~8200 行）已实现完整的 7 页面管理界面。桌面端（egui，9 个 rs 文件，~1200 行）功能滞后。本方案将 WebUI 的界面改进反向移植到桌面 GUI，同时修复 ep-core 的架构缺陷，最终交付一个可在 Arch Linux 上安装的程序包。

### 1.1 目标

- 桌面 GUI 功能对齐 WebUI（模型管理、可视化管线、统计仪表盘、任务中心、主题）
- 修复 ep-core 3 个高优 + 2 个中优架构问题
- 校准现有 5 个模块的 HuggingFace 模型声明
- GUI 代码同时适配 Windows 和 Linux（跨平台为约束，非独立任务）
- 用真实媒体文件完成全流程 E2E 测试
- 交付 Arch Linux 可安装包（PKGBUILD + .pkg.tar.zst）

### 1.2 排除

- 新模块 / 新类别 / 新模型拓展
- ep-daemon 任何修改（桌面端直连 ep-core，不走 HTTP）
- 服务器概念（IP 过滤、HTTP 端口、WebSocket）引入桌面端
- 模型文件入 git（models/ 已 gitignore）

---

## 2. 现状分析

### 2.1 代码规模

| | 桌面端 (ep-desktop) | WebUI (ep-webui) |
|---|---|---|
| 文件数 | 9 个 .rs | 56 个 .ts/.tsx |
| 代码量 | ~1200 行 | ~8200 行 |
| 页面数 | 5 | 7 + 404 |

### 2.2 功能差距

| 功能 | WebUI | 桌面端 | 移植策略 |
|---|---|---|---|
| 模型管理页（下载/导入/手动复制） | ✅ 420 行 | ❌ 完全缺失 | **新增** `pages/models.rs` |
| 可视化管线编辑器 | ✅ React Flow 687 行 + 3 组件 | ⚠️ 仅 TOML 文本表格 | **重写** `pages/pipeline_editor.rs` |
| 仪表盘统计卡片 + 依赖报告 | ✅ 497 行 | ⚠️ 仅设备卡 + 模块表 | **增强** `pages/dashboard.rs` |
| 任务中心（管线任务 + 运行时间） | ✅ 390 行 | ⚠️ 仅模块状态表 | **增强** `pages/tasks.rs` |
| 模块详情（能力/模型/依赖） | ✅ 449 行 | ⚠️ 有基础面板 | **增强** `pages/modules.rs` |
| 深色/浅色主题 | ✅ CSS 变量 + localStorage | ❌ egui 默认 | **新增** `theme.rs` |
| Toast 通知 | ✅ sonner | ⚠️ 仅状态栏文字 | **新增** `toast.rs` |
| 文件选择对话框 | N/A（浏览器原生） | ❌ 缺失 | **新增** `rfd` crate |

### 2.3 ep-core 架构缺陷（来自边界分析）

#### 高优先级

| # | 问题 | 现状 | 影响 |
|---|---|---|---|
| H1 | 日志捕获断开 | `child.stdout.take()` 后丢弃管道句柄，`log_buffer` 永远为空 | UI 的 LogViewer 无数据 |
| H2 | 健康检查未实现 | `monitor_process` 仅检查进程退出，不轮询 `/health` | Starting → Running 转换不可靠 |
| H3 | URL 模型下载缺失 | `source = "url"` 返回 `sys.exit(1)` 占位 | deep-filter / paddleocr / rembg 3 个模块无法下载模型 |

#### 中优先级

| # | 问题 | 现状 | 影响 |
|---|---|---|---|
| M1 | start_command 平台路径硬编码 | 模块 TOML 写死 `bin/python`（Linux） | Windows 上无法启动模块 |
| M2 | ModuleCategory 硬编码枚举 | 新类别需改 Rust 代码重编译 | 第三方的新类别模块无法接入 |

### 2.4 模块与主体边界

```
┌─────────────────────────────────────────────────────────────┐
│  ep-core (Rust)                                             │
│                                                             │
│  发现 → 校验 → venv创建 → 模型下载 → 进程管理 → 健康检查    │
│  端口分配 → 设备分配 → 管线编排 → HTTP调用 → 产物管理        │
│                                                             │
│  通信契约: 环境变量(入) + REST API(出)                       │
├─────────────────────────────────────────────────────────────┤
│  模块 (Python/Native)                                       │
│                                                             │
│  读取环境变量 → 加载模型 → 暴露 REST 端点 → 执行推理         │
│  模型格式处理 → 框架差异消化 → 输出标准化                    │
└─────────────────────────────────────────────────────────────┘
```

**核心分界线**：ep-core 管「生命周期基础设施」，模块管「AI 领域逻辑」。本次修复不改变此边界，仅修补 ep-core 侧的未完成实现。

### 2.5 Linux 兼容性现状

- ✅ ep-core 已完成 Linux 适配（路径、信号、平台检测）
- ✅ `cargo check -p ep-desktop` 在 RHEL 9.8 上通过（23.37s）
- ✅ 134 测试全绿，clippy 零警告
- ✅ CJK 字体路径已包含 Linux 路径
- ✅ Rust 1.97.1 已安装
- ⚠️ 未执行过 release 构建
- ⚠️ 无 Arch Linux 打包基础设施

---

## 3. 模型管理 UX 设计

桌面端模型管理页支持三条获取路径：

```
┌─────────────────────────────────────────────────────────┐
│  模型管理                                    [刷新]      │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  📂 模型缓存目录: /server/EntryPoint/models   [打开目录] │
│                                                         │
│  ┌─ faster-whisper ──────────────────────────────────┐  │
│  │                                                   │  │
│  │  Whisper Large V3                                 │  │
│  │  来源: huggingface / Systran/faster-whisper-large-v3│ │
│  │  状态: 🟢 就绪  大小: 2.9 GB                      │  │
│  │  [删除]                                           │  │
│  │                                                   │  │
│  │  Whisper Medium                                   │  │
│  │  来源: huggingface / Systran/faster-whisper-medium│  │
│  │  状态: 🔴 缺失                                    │  │
│  │  [⬇ 下载]  [📁 导入本地模型]                      │  │
│  │                                                   │  │
│  └───────────────────────────────────────────────────┘  │
│                                                         │
│  💡 手动复制: 将模型文件夹放入上述缓存目录，点击刷新即可  │
│     识别。文件夹名需与 module.toml 中 target_dir 一致。   │
└─────────────────────────────────────────────────────────┘
```

| 路径 | 触发 | 实现 |
|---|---|---|
| 在线下载 | [⬇ 下载] | `ModelManager::build_download_command()` → `tokio::process::Command` 在模块 venv 中执行 → 解析 stdout 回传进度 → UI 进度条 |
| 本地导入 | [📁 导入] | `rfd::FileDialog::pick_folder()` → 复制到 `cache_dir/<target_dir>` → 写入 `.ep_meta.json`(source="local") → 刷新 |
| 手动复制 | 用户自行操作 | 页面显示缓存目录绝对路径 + [打开目录]（`xdg-open` / `explorer`）+ 引导文字 |

### 新增消息/命令

```rust
// app.rs 新增
AppMsg::ModelDownloadProgress(String, f32),    // (model_id, 0.0~1.0)
AppMsg::ModelDownloadFinished(String, bool),    // (model_id, success)

AppCmd::DownloadModel(String, String),          // (module_id, model_id)
AppCmd::DeleteModel(String),                    // (target_dir)
AppCmd::ImportModel(String, PathBuf),           // (target_dir, source_path)
```

### 新增依赖

```toml
# ep-desktop/Cargo.toml
rfd = "0.15"    # 原生文件/文件夹选择对话框（跨平台）
```

---

## 4. 跨平台规则

所有 Agent 写代码时必须遵守，不单独排期：

| # | 规则 |
|---|---|
| 1 | 路径使用 `std::path::Path`，不硬编码分隔符 |
| 2 | 字体保留 `main.rs` 中 Windows + Linux 双路径列表 |
| 3 | 文件对话框用 `rfd` crate（原生跨平台），不自造 |
| 4 | 打开目录：`cfg!(windows)` → `explorer`，否则 → `xdg-open` |
| 5 | venv python：Windows 用 `Scripts/python.exe`，Linux 用 `bin/python` |
| 6 | 不引入仅单平台可用的 crate 或 API |
| 7 | `cargo check` 在 Linux 上验证（当前环境），Windows 兼容性靠代码审查 |

---

## 5. 多代理并行开发规则

### 规则 1：文件独占

同一 Phase 内，每个 Agent 拥有排他的文件写权限。任何两个并行 Agent 不得编辑同一文件。

### 规则 2：接口契约

Phase 1 定义 API 表面（类型签名 + 方法），Phase 2 消费。不允许反向依赖。Agent 之间通过已编译的 ep-core 公共接口通信。

### 规则 3：验证门控

每个 Agent 完成后必须运行：

```bash
cargo check -p ep-desktop    # GUI Agent
cargo test -p ep-core        # Core Agent
cargo clippy -p <crate>      # 所有 Agent
```

未通过不得报告完成。

### 规则 4：隔离工作树

每个 Agent 在独立 git worktree 中工作（`isolation: "worktree"`），避免文件系统冲突。编排者在 Phase 间合并。

### 规则 5：共享文件仲裁

`app.rs`（导航枚举、消息类型）、`lib.rs`（导出）、`pages/mod.rs`（模块声明）是编排者独占文件。Agent 在交付物中说明需要哪些修改，由编排者统一执行。

### 规则 6：最大并行

同一 Phase 内所有 Agent 同时启动（单条消息多个 Agent 调用），不等待彼此完成。

### 文件所有权矩阵

| Agent | 独占文件 | 只读引用 |
|---|---|---|
| A (CoreFix) | `process.rs`, `health.rs`, `types.rs`, `module/manifest.rs` | config.rs, lib.rs |
| B (ModelFix) | `model.rs`, `modules/*/module.toml` | types.rs, config.rs |
| C (ModelsPage) | `pages/models.rs` (新建) | app.rs, ep-core |
| D (DashboardV2) | `pages/dashboard.rs` | app.rs, ep-core |
| E (PipelineViz) | `pages/pipeline_editor.rs` | ep-core/pipeline |
| F (TasksV2) | `pages/tasks.rs` | app.rs, ep-core |
| G (ThemeToast) | `theme.rs` (新建), `toast.rs` (新建), `pages/settings.rs` | app.rs |
| H (ModuleDetailV2) | `pages/modules.rs` | app.rs, ep-core |
| I (ArchPacker) | `packaging/` (新建) | 全部（只读） |

---

## 6. 执行计划

### Phase 0 — 环境验证 ✅ 已完成

- `cargo check -p ep-desktop` 通过（23.37s）
- `cargo test` 134/134 通过
- `cargo clippy` 零警告
- 无需额外安装系统依赖

### Phase 1 — ep-core 修复（2 Agent 并行）

#### Agent A (CoreFix)

**独占**：`process.rs`, `health.rs`, `types.rs`, `module/manifest.rs`

| 任务 | 细节 |
|---|---|
| H1 日志捕获 | spawn stdout/stderr 读取任务，逐行写入 `log_buffer` + channel 回传 |
| H2 健康检查 | 进程启动后按 `ready_timeout_secs` 轮询 `GET /health`，通过后才标记 Running |
| M1 平台路径 | `build_start_command` 按 `cfg!(windows)` 选择 `Scripts/python.exe` 或 `bin/python`；模块只需声明 `entrypoint` |
| M2 类别可扩展 | `ModuleCategory` 改为 `enum { ..., Other(String) }`，manifest 解析兼容未知类别 |

**验证**：`cargo test -p ep-core && cargo clippy -p ep-core`

#### Agent B (ModelFix)

**独占**：`model.rs`, `modules/*/module.toml`

| 任务 | 细节 |
|---|---|
| H3 URL 下载 | 实现 reqwest 下载 + 进度回传；或将 `source = "url"` 改为 HF/MS 源 |
| HF 数据校准 | 用 HF API 校验 5 个模块的 repo_id 存在性、模型大小 |
| module.toml 修正 | 更新不正确的声明（repo_id、vram_estimate_mb 等） |

**验证**：`cargo test -p ep-core && cargo clippy -p ep-core`

### Phase 2 — GUI 页面（4 Agent 并行）

> 依赖：Phase 1 完成

#### Agent C (ModelsPage)

**独占**：`pages/models.rs`（新建）

- 按模块分组（CollapsingHeader），每个模型：名称、来源、repo_id、状态、大小
- [⬇ 下载] → `AppCmd::DownloadModel`
- [📁 导入] → `rfd::FileDialog::pick_folder()` → `AppCmd::ImportModel`
- [删除] → 确认对话框 → `AppCmd::DeleteModel`
- 下载中显示进度条
- 顶部：缓存目录路径 + [打开目录]
- 底部：手动复制引导文字
- **交付说明**：请编排者添加 `Page::Models`、导航项、AppCmd/AppMsg 变体

#### Agent D (DashboardV2)

**独占**：`pages/dashboard.rs`

- 统计卡片行（4 格）：设备数 / 模块数 / 运行中 / 错误数
- 依赖报告区：ffmpeg 状态 + torch CUDA 状态（调用 `DepReport::check_all`）
- 保留现有设备卡片 + 模块表

#### Agent E (PipelineViz)

**独占**：`pages/pipeline_editor.rs`（重写）

- 自研 egui 节点画布（`egui::Painter`，不引入新 crate）：
  - 节点矩形（圆角、标题栏、端口圆点）
  - 贝塞尔曲线连线
  - 鼠标拖拽移动节点 / 端口拖拽创建连线
  - 画布平移（中键拖拽）/ 缩放（滚轮）
- 左侧节点面板：可用模块 capabilities + 内置节点
- 右侧参数面板：选中节点参数编辑
- 工具栏：加载 TOML / 保存 TOML / 验证
- 节点状态着色：灰=等待 / 蓝=运行 / 绿=完成 / 红=失败
- 保留 TOML 兼容

#### Agent F (TasksV2)

**独占**：`pages/tasks.rs`（重写）

- 管线任务列表：ID、名称、状态、进度（completed/total 节点）、耗时
- 任务详情展开：各节点执行状态（Pending/Running/Completed/Failed + 颜色）
- 模块服务状态区 + 运行时间列 + 分类标签
- 调用 `PipelineRunner::list_tasks()` / `get_task_detail()`

### Phase 3 — 打磨（2 Agent 并行）

> 依赖：Phase 2 完成

#### Agent G (ThemeToast)

**独占**：`theme.rs`（新建）, `toast.rs`（新建）, `pages/settings.rs`

- `theme.rs`：映射 DESIGN_SYSTEM.md 色板到 `egui::Visuals`
  - 深色（默认）：background `#0a0a0a`, card `#1a1a1a`, primary `#3b82f6`
  - 浅色：background `#ffffff`, card `#ffffff`, primary `#3b82f6`
  - 状态色：running 绿 / stopped 灰 / starting 蓝 / error 红
  - `pub fn apply_theme(ctx: &egui::Context, dark: bool)`
- `toast.rs`：右下角弹出矩形，success/error/info，3 秒自动消失
- `settings.rs`：界面区增加主题切换 ComboBox
- **交付说明**：请编排者在 `app.rs` 中集成 toast 状态和主题应用

#### Agent H (ModuleDetailV2)

**独占**：`pages/modules.rs`

- Capabilities 列表（名称、input_type → output_type、参数 schema）
- 关联模型信息（调用 `ModelManager::list_all_models` 过滤当前模块）
- 依赖检测结果
- 日志查看器微调

### Phase 4 — 集成 + 打包

> 依赖：Phase 3 完成

#### 编排者

1. 合并所有 Agent worktree 到主分支
2. 修改 `app.rs`：
   - `Page` 枚举添加 `Models`
   - `NAV_ITEMS` 添加 `(Page::Models, "📦", "模型")`
   - `CentralPanel` 分发添加 `Page::Models`
   - 集成 Toast 状态 + 主题应用
   - AppCmd/AppMsg 新增变体
3. 修改 `lib.rs`、`pages/mod.rs`
4. `background_loop` 中实现 DownloadModel / DeleteModel / ImportModel 命令处理
5. 全量 `cargo clippy && cargo test && cargo build --release`
6. 修复集成问题

#### Agent I (ArchPacker)

**独占**：`packaging/`（新建）

- `PKGBUILD`：
  - `pkgname=entrypoint`, `pkgver=0.1.0`, `arch=('x86_64')`
  - `depends=('gcc-libs' 'libxkbcommon' 'wayland' 'libxdo' 'fontconfig')`
  - `makedepends=('rust' 'npm')`
  - `optdepends=('nvidia-utils: CUDA GPU 支持' 'ffmpeg: 视频处理' 'uv: Python 环境管理')`
  - build(): `cargo build --release` + 前端构建
  - package(): 安装二进制、systemd 服务、默认配置、.desktop 文件
- `entrypoint.desktop`：桌面启动器
- `entrypoint.install`：post-install 提示
- 在 RHEL 上编写 + 语法验证；实际 `makepkg` 需在 Arch 环境执行

### Phase 5 — E2E 测试 + 提交

#### 全流程测试

```
测试管线：音频 → ASR → 文本输出

1. 生成测试音频
   ffmpeg -f lavfi -i "sine=frequency=440:duration=5" -ar 16000 -ac 1 test_input.wav

2. 下载模型
   faster-whisper-tiny (~75MB, Systran/faster-whisper-tiny)
   通过模型管理 API 触发（验证 ModelManager 链路）

3. 启动模块
   通过 ProcessManager 启动 faster-whisper（验证健康检查 → Running）

4. 执行推理
   调用模块 /predict/transcribe（验证管线执行链路）

5. 验证
   - 进程启动成功（健康检查通过）
   - 日志捕获有输出（log_buffer 非空）
   - 推理完成（产物存在）
   - 不检查识别内容正确性

6. 清理
   - 停止模块
   - 删除测试音频和产物
   - 模型文件保留（已 gitignore）或删除
```

#### 提交

按功能分 commit（~8 个）：

```
feat(core): implement log capture + health check polling
feat(core): implement URL model download + calibrate module manifests
feat(core): extensible ModuleCategory + platform-adaptive start_command
feat(desktop): add models management page with download/import/manual-copy
feat(desktop): visual pipeline editor with node canvas
feat(desktop): enhanced dashboard with stats + dependency report
feat(desktop): enhanced tasks center + module detail + theme + toast
feat(pkg): Arch Linux PKGBUILD + packaging infrastructure
```

更新 `PROGRESS.md`、`README.md`。

---

## 7. 并行度与时间线

```
Phase 0  ██                              编排者（已完成）
Phase 1  ████████████                    Agent A ∥ B           并行度 2
Phase 2  ████████████████████            Agent C ∥ D ∥ E ∥ F   并行度 4 ← 峰值
Phase 3  ██████████                      Agent G ∥ H           并行度 2
Phase 4  ████████████                    编排者 + Agent I
Phase 5  ████████                        编排者（E2E + commit）

Agent 总数: 9    峰值并行: 4    Phase 总数: 6（含 0）
```

---

## 8. 风险与缓解

| 风险 | 概率 | 缓解 |
|---|---|---|
| eframe release 构建链接失败 | 低 | Phase 1 Agent 验证 release build |
| 自研节点编辑器复杂度 | 中 | 先最小可用版（静态布局+连线+拖拽），再迭代 |
| 多 Agent 合并后编译错误 | 中 | Phase 4 预留充足集成修复时间 |
| RHEL 上无法运行 makepkg | 确定 | 提供 PKGBUILD + 构建说明，Arch 环境构建 |
| process.rs 并发修改冲突 | 低 | A+C 合并为单 Agent |
| HF 模型下载网络问题 | 中 | 使用 127.0.0.1:20171 HTTP 代理 |
| E2E 测试模块启动失败 | 中 | 使用最小的 tiny 模型，降低资源需求 |

---

## 9. 交付物

| 交付物 | 格式 | 说明 |
|---|---|---|
| 源码 | Git commits (~8) | 按功能分，不含模型/测试文件 |
| Arch 包定义 | `packaging/PKGBUILD` + `.desktop` + `.install` | Arch 打包基础设施 |
| Arch 包 | `entrypoint-0.1.0-1-x86_64.pkg.tar.zst` | 在 Arch 环境构建 |
| 构建脚本 | `scripts/build-desktop.sh` | 桌面端 release 构建 |
| E2E 验证 | 全流程跑通 | 真实媒体文件，不检查输出内容 |
| 文档 | `PROGRESS.md`, `README.md` 更新 | 记录反向移植工作 |

---

## 10. 未来工作（本次不做，记录备查）

来自边界分析的低优先级建议：

| 建议 | 理由 |
|---|---|
| `ep-adapter-sdk` Python 包 | 消除 FastAPI boilerplate，第三方开发者只需实现 `predict()` |
| 模块验证工具 `ep validate` | 自动检查 module.toml + adapter.py 合规性 |
| 跨模块模型共享（模型注册表） | 以 repo_id 去重，避免同一模型下载多次 |
| Docker 运行时 | 设计中提到 `runtime.type = "docker"` 但未实现 |
| 模块脚手架 `ep new-module` | 生成模板目录 |
| `start_command` 变量替换统一 | 设计文档用小写 `{port}`，实际用大写 `{ROOT}`，需统一 |
