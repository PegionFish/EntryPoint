# EntryPoint 开发进度

## Wave -1 — ✅ 完成
### 完成
- [x] ep-ui 重命名为 ep-desktop
- [x] 创建 ep-daemon crate（axum HTTP server 骨架）
- [x] 创建 ep-webui crate（静态占位页）
- [x] workspace Cargo.toml 更新为 4 crate 架构
### Git
- commit: `08b4d1c` — "refactor(wave--1): split workspace into daemon + desktop + webui architecture"

## Wave 0 — ✅ 完成
### 完成
- [x] types.rs 添加 ModuleProcess, DeviceScheduler, PipelineRunner trait
- [x] 创建骨架文件: compute/scheduler.rs, module/lifecycle.rs, health.rs, pipeline/runner.rs
- [x] lib.rs 声明所有新模块
### Git
- commit: `5833cd5` — "chore(wave-0): define shared traits + skeleton files for parallel agents"

## Wave 1a — ✅ 完成
### 完成
- [x] Agent A (ProcessForge): 真实进程管理 + HTTP 健康检查（17 测试）
- [x] Agent B (DeviceMaster): 4 种设备调度策略（9 测试）
### 编译状态
- cargo check: ✅
- cargo test: 94/94 passed
### Git
- commit: `6b121ba` — "feat(wave-1a/agent-a): implement real process management + health check"
- commit: `8124f25` — "feat(wave-1a/agent-b): implement compute device scheduler with 4 strategies"

## Wave 1b — ✅ 完成
### 完成
- [x] Agent C (PipelineCrusher): 管线执行引擎 + builtin 节点支持（5 新测试）
### 编译状态
- cargo check: ✅
- cargo test: 99/99 passed
### Git
- commit: `5332517` — "feat(wave-1b/agent-c): implement pipeline execution engine with builtin node support"

## Wave 2 — ✅ 完成
### 完成
- [x] Agent D (UIWeaver): ep-desktop UI 重写，直连 ep-core（8 文件，+912/-300 行）
- [x] Agent E (EnvRunner): 模块生命周期 + 环境/模型管理增强（12 新测试）
- [x] Agent D2 (DaemonForge): ep-daemon 完整 REST API + WebSocket（7 测试）
### 编译状态
- cargo check: ✅
- cargo test: 118/118 passed (111 ep-core + 7 ep-daemon)
### Git
- commit: `6376cdb` — "feat(wave-2/agent-d): rewrite ep-desktop UI with real ep-core integration"
- commit: `664df09` — "feat(wave-2/agent-d2): implement full REST API + WebSocket skeleton for ep-daemon"
- commit: `32f2d84` — "feat(wave-2/agent-e): implement module lifecycle + env/model enhancements"

## Wave 3 — ✅ 完成
### 完成
- [x] cargo clippy — 零警告
- [x] cargo test — 118/118 passed
- [x] cargo build --release — 成功（47.73s）
### Git
- commit: `c2210bd` — "chore(wave-3): clippy fixes + integration smoke + PROGRESS.md"

## Wave 4 — ✅ 完成
### 完成
- [x] Agent F (TestHawk): 13 个集成测试（模块生命周期/管线/设备调度）
- [x] Agent G (PolishCat): 默认配置 + 2 个示例管线
### 编译状态
- cargo test: 131/131 passed (111 unit + 13 integration + 7 daemon)
### Git
- commit: `2c5c9e9` — "test(wave-4/agent-f): add integration tests"
- commit: `dfc33b1` — "feat(wave-4/agent-g): add default config + example pipelines"

## Wave 5 — ✅ 完成
### 完成
- [x] Daemon API 测试: /health, /devices, /modules, /config 全部 200
- [x] Desktop GUI 测试: 启动/导航/GPU检测/设置页面 全部通过
- [x] RTX 5090 D 正确检测 (32607 MB, 42°C)
- [x] 测试报告: reports/e2e_test_report.md
### Git
- commit: TBD — "test(wave-5): e2e test results"

## Wave 6 — ✅ 完成
### 完成
- [x] cargo check ✅
- [x] cargo test — 131/131 passed
- [x] cargo clippy — 0 warnings
- [x] cargo build --release — 成功
- [x] 实机测试通过
- [x] 最终报告

---

## WebUI 实现（2026-07-28 ~ 2026-07-29）

### Wave 0 (WebUI) — ✅ 完成
#### 完成
- [x] 开发环境搭建：Rust 1.97.1, uv 0.11.33, ffmpeg 5.1.10
- [x] Node.js 20 (v20.20.2) 确认可用
- [x] 验证 cargo test / clippy 基线通过

### Wave 1 (WebUI) — ✅ 完成
#### 完成
- [x] Linux 适配：路径处理、信号管理、平台检测（134 tests, 0 clippy warnings）
- [x] React 前端脚手架：Vite + React 19 + TypeScript + TailwindCSS 4 + shadcn/ui（52 files）
- [x] 部署脚本：scripts/build.sh, scripts/install-service.sh, scripts/entrypoint.service
- [x] 桌面端 CJK 字体支持（Noto Sans SC）

### Wave 2 (WebUI) — ✅ 完成
#### 完成
- [x] 7 个业务页面实现：仪表盘、模块管理、模块详情、管线编辑器、任务中心、模型管理、设置
- [x] 共享组件：status-badge, confirm-dialog, empty-state, loading-skeleton, device-card, module-card, log-viewer
- [x] 自定义 hooks：use-polling, use-module-actions, use-ws-state 等
- [x] 前端规模：25 个核心业务文件，5376 行新增代码

### Wave 3 (WebUI) — ✅ 完成
#### 完成
- [x] API 路由修复：SPA fallback + 静态资源服务
- [x] Release 构建验证：cargo build --release + npm run build
- [x] E2E 测试：全部 API 端点 200，页面组件 TS 检查通过
- [x] 测试报告：reports/webui_test_report.md

### Wave 4 (WebUI) — ✅ 完成
#### 完成
- [x] 文档更新：README, PROGRESS, DESIGN, docs/DEPLOYMENT.md

---

## 桌面 GUI 反向移植 + 架构优化（2026-07-30）

### Phase 1 — ep-core 架构修复 ✅
#### 完成
- [x] H1 日志捕获：spawn stdout/stderr reader task → channel → log_buffer（原：丢弃管道句柄）
- [x] H2 健康检查：monitor_process 轮询 /health 端点，Starting→Running 依赖 200 响应
- [x] H3 URL 模型下载：Python urllib 实现（tar.gz 解压 + 单文件 + auto 占位）
- [x] M1 平台路径：注入 {venv_python} 变量（Windows: Scripts/python.exe, Linux: bin/python）
- [x] M2 类别扩展：ModuleCategory::Other(String) 支持第三方类别无需重编译
#### 验证
- cargo test: 全部通过
- cargo clippy: 零警告
#### Git
- commit: `5e7e370` — "feat(core): implement log capture, health check polling, URL model download, extensible ModuleCategory, platform-adaptive venv python"

### Phase 2-4 — 桌面 GUI 页面 + 集成 ✅
#### 完成
- [x] 模型管理页（pages/models.rs）：按模块分组、下载/导入/删除、状态徽章、大小格式化
- [x] 可视化管线编辑器（pages/pipeline_editor.rs 重写）：egui Painter 节点画布、贝塞尔连线、拖拽/缩放
- [x] 仪表盘增强（pages/dashboard.rs）：统计卡片 + 依赖检测报告区
- [x] 任务中心增强（pages/tasks.rs）：管线任务列表 + 节点详情展开
- [x] 主题系统（theme.rs）：深色/浅色 Visuals，DESIGN_SYSTEM.md 色板映射
- [x] Toast 通知（toast.rs）：右下角弹出、3 秒自动消失、淡出动画
- [x] app.rs 集成：Models 页面、Toast、主题切换、新 AppCmd/AppMsg 变体
- [x] main.rs：DownloadModel/DeleteModel/ImportModel/RefreshDeps 命令处理
#### 验证
- cargo check: ✅
- cargo clippy: 零警告
#### Git
- commit: `daf24c5` — "feat(desktop): add models page, visual pipeline editor, enhanced dashboard/tasks, theme + toast"

### Arch Linux 打包 ✅
#### 完成
- [x] packaging/PKGBUILD：cargo release + WebUI 前端构建 + systemd 服务
- [x] packaging/entrypoint.desktop：freedesktop 启动器
- [x] packaging/entrypoint.install：post-install/upgrade/remove 钩子
- [x] packaging/entrypoint.service：systemd 单元（安全加固）
- [x] scripts/build-desktop.sh：一键 release 构建脚本
#### Git
- commit: `6742410` — "feat(pkg): Arch Linux PKGBUILD + packaging infrastructure + build-desktop.sh"

### E2E 全流程测试 ✅
#### 测试流程
1. 从 /server/samba/Media 提取 10s FLAC → WAV 测试音频
2. 重建 faster-whisper Linux venv（uv venv + uv pip install）
3. 修复 adapter.py Python 3.9 兼容性（PEP 604 → typing.Optional/Union）
4. 修复 compute type 回退（float16 → int8，Tesla P4 不支持 FP16）
5. Daemon 启动 → 模块发现（5 模块）→ 模块启动 → 健康检查通过 → ASR 推理完成
#### 结果
- 推理状态: completed
- 耗时: 45.4s（CPU, large-v3 模型）
- 测试音频为 BGM 纯音乐，无语音内容是预期结果
#### Git
- commit: `f3371c3` — "fix(module): Python 3.9 compatibility + compute type fallback for faster-whisper adapter"

---

## UI/UX 现代化改造（2026-08-03）

> 目标：桌面 GUI 现代视觉（对齐 WebUI 设计系统）+ 全窗口尺寸自适应；WebUI 响应式修补。
> 方式：13 个并行子代理分批执行（Wave B 2 代理 + Wave C 6 页面代理 + WebUI 巡检/修复 3 代理）。

### 批次① 设计基础 ✅
- [x] `ui/palette.rs`：Palette 设计令牌（深/浅色板映射 DESIGN_SYSTEM.md）+ service_status 唯一权威状态映射（删除 4 份矛盾定义）
- [x] `ui/components.rs`：卡片/徽章/页头/空态/响应式网格/确认对话框/主题按钮
- [x] theme.rs 重构：基于 Palette 的 Visuals + apply_font_size（TextStyle 等比缩放）
#### Git
- commit: `f8e98fd` — "feat(ui): 设计基础 — 设计令牌 Palette + 共享组件库 + 主题系统重构（批次①）"

### Wave B 外壳 + 分辨率自适应 ✅
- [x] 设置页「UI 缩放/字号」死设置复活：set_zoom_factor + TextStyle 缩放，修改即时生效
- [x] 窗口最小尺寸 900×600 → 720×480；超屏 92% 自动收缩
- [x] 侧导航 <1000px 收折为图标栏（PanelState 缓存清除实现真响应式）+ 激活指示条
- [x] 主题切换持久化到 config.general.theme；移除顶部菜单栏；Toast 主题感知
- [x] 模型管理全接线：启动自动扫描模型 + 依赖检测；刷新/下载(execute_download)/导入(import_model)/删除(remove_dir_all) 全部接通
#### Git
- commit: `9eedf10` — "feat(desktop): 外壳现代化 + 分辨率自适应（Wave B）"

### Wave C 六页面重做 ✅（6 并行代理）
- [x] dashboard：统计/设备卡响应式网格、显存进度条着色、状态徽章、横滚表格
- [x] modules：响应式 master-detail、停止/重启确认框、日志区保留
- [x] models：刷新/下载/删除/导入按钮全部启用 + 路径导入框 + 删除确认框
- [x] tasks：任务卡片化 + 徽章 + 进度条 + 横滚表格
- [x] settings：六分区卡片化 + Toast 反馈（签名改用 ToastManager）
- [x] pipeline_editor：全主题化画布（浅色修复）、−/＋/⤢适配 工具栏、<640px 隐藏侧栏
#### Git
- commit: `cbf0f60` — "feat(desktop): 六页面现代化重做（Wave C）"

### WebUI 响应式修补 ✅（巡检代理 + 2 修复代理）
- [x] P0：全局侧栏 <lg 隐藏 + 汉堡菜单 + Sheet 抽屉（复用 shadcn sheet）
- [x] P1：管线页窄屏节点库抽屉化、参数面板 overlay 化；工具栏图标化降级
- [x] P2：index.css 新增 23 个语义色令牌（dtype/cat/http/node）替换 24 处硬编码调色板色；page-container 断点内边距；tasks 统计条 wrap；图例窄屏隐藏；节点库触屏点击添加
- 验证：tsc + vite build 通过
#### Git
- commit: `02d3b9b` — "feat(webui): 响应式修复 — 侧栏抽屉收折(P0)/管线页窄屏overlay/工具栏降级/语义色令牌化/触屏添加节点"

### 验证
- cargo check / clippy（-D warnings）：零警告；cargo test -p ep-desktop：5/5 通过
- debug 版启动冒烟测试：进程存活、CJK 字体加载正常
- npm run build：通过

---

## 最终统计

| 指标 | 值 |
|---|---|
| Rust 测试数 | 134 (111 unit + 13 integration + 7 daemon + 3 linux) |
| Clippy warnings | 0 |
| Rust 源文件数 | ~60 .rs files |
| 前端源文件数 | 57 (.ts/.tsx) |
| 前端代码行数 | ~8200 行 |
| 桌面端代码行数 | ~2800 行 (含反向移植) |
| Crate 数 | 4 (ep-core, ep-daemon, ep-desktop, ep-webui) |
| Release 构建时间 | ~3m 34s (含前端) |
| Git commits | 20+ |
| E2E 测试 | ✅ 全流程通过 (ASR 推理) |

---

## TODO（待办）

- [ ] `build.sh` 支持 `--distro <发行版>` 参数：默认自动检测当前发行版选择包格式，用户可显式指定目标发行版
  - 当前实现：`--distro` 仅用于决定包格式（deb / rpm / PKGBUILD），未识别时只产出 tar.gz 兜底包
  - 待实现：具体发行版适配（glibc 版本约束、依赖包名差异、系统服务与安全策略等）
