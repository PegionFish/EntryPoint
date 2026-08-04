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

## WebUI 端到端可用性与模型管理强化（2026-08-03 ~ 2026-08-04）

> 目标：WebUI 从"页面可见"升级为"端到端真实可用"——打通 WS / 日志 / 管线执行 / 模型全生命周期，以真实浏览器 + 真实媒体 E2E 验收。
> 方式：4 并行审计代理摸底 → 四波并行子代理实施（Wave 1 ×3 / Wave 2 ×6 / Wave 3 ×5 / Wave 4 ×4）→ 集成门禁与缺陷修复。

### 审计发现（4 并行审计代理）✅
#### P0（4 项）
- [x] WebSocket 三重断链：URL 不匹配 / 消息格式不一致 / 前端数据源未接
- [x] 日志广播丢行 + 重复
- [x] 管线执行零接线：执行引擎在 ep-core，daemon 无入口
- [x] daemon 缺模型下载 / 删除 / 上传 API
#### 关键 P1
- [x] PUT /api/config 不落盘；SPA fallback 恒 404
- [x] CUDA 依赖契约错（永远"未安装"）；GPU 数据静态不刷新
- [x] default_source 死配置；下载不写 meta；桌面端导入 target_dir bug
- [x] HF 下载 3 倍磁盘膨胀（faster-whisper-large-v3 实测 8.7G / 有效 2.9G）
- [x] 全仓库无代理配置；412/409 全新安装死锁；df3 URL 死链 等

### Wave 1 地基修复 ✅（3 代理并行）
- [x] ep-core：`[[models.mirrors]]` 双源清单（faster-whisper 3 模型 ↔ ModelScope pengzhendong/*，在线核实）
- [x] ep-core：`[network]` 代理配置节 + 三处子进程注入；下载写 meta；import target_dir 修复
- [x] ep-core：磁盘轮询式下载进度（broadcast）；check_update_available（HF/MS API）；cleanup_hf_cache；reqwest no_proxy
- [x] daemon：/ws 聚合端点（type 标签）；日志增量去重广播；SPA fallback 200 化 + /api JSON 404
- [x] daemon：配置落盘；设备周期刷新；状态规范化 + 中文错误信息；Wave 2 全路由骨架
- [x] 前端：契约修复（deps 字段 / 死代码清理）；日志查看器（搜索/过滤/导出/高亮）；确认框接线；12 个新 API 契约预置

### Wave 2 后端能力 ✅（6 代理并行）
- [x] 模型 API：download（202 + WS 进度 + 选源 + 防重）/ delete / check-update / downloads 列表（15 测试）
- [x] multipart 上传：文件夹多文件 + zip/tar.gz 双形态、流式落盘、路径清洗 + zip-slip 防御、归档剥层（19 测试）
- [x] 管线 CRUD：React Flow JSON ↔ TOML 桥接、builtin 保护、示例真实往返（20 测试）
- [x] 执行引擎接线：PipelineRunnerImpl + 任务注册表 + WS 进度 + 产物 302 下载（23 测试，含真实 HTTP 冒烟）
- [x] 桌面端：rfd 文件夹导入；下载后台化 + 进度条 + 取消；双源选择；更新管理；代理设置分区（Windows 交叉编译验证）

### Wave 3 WebUI 页面 ✅（5 代理并行）
- [x] 模型页全功能：XHR 真实进度上传 / 拖拽文件夹 / zip、选源下载、删除确认、更新检查
- [x] 管线页接通服务端：管线库 / 保存 / 另存为 / 执行对话框 / 必填校验 / WS 节点状态 / 端口名归一
- [x] 任务页真实数据：节点详情 / 产物下载 / 空态引导
- [x] 设置页：代理分区 + 校验、误导项清理（English/system）、NumberField 加固
- [x] 仪表盘 / 模块页打磨：异常计数 / 快捷启动 / not_ready 引导 / 日志截断提示

### Wave 4 集成门禁 + E2E ✅（4 代理并行 + 门禁修复）
- [x] API 冒烟 40 项通过（唯一偏差 D1 已修：模块详情/import 404 中文消息）
- [x] 真实浏览器巡测：Playwright Chromium headless（手工补齐 6 个系统库），8 页面零控制台错误；修复内置管线边不渲染（端口名归一）、主题下拉不同步
- [x] E2E 途中修复的产品缺陷：ffmpeg {input}/{output} 占位符失配（两条内置管线必挂）、output_extension 未尊重、faster-whisper CUDA→CPU 设备级回退、ASR SRT 导出（output_format/output_path 模块产物协议）、file_output extension 派生路径、412 下载死锁→自动 venv 准备、df3 URL 死链→HF 镜像（Serkan007/DeepFilterNet3-ONNX，内容已验证）、default_source 接线
- [x] 真实媒体 E2E：video_to_srt 全流程通过（15s 视频 → WAV → ASR large-v3 CPU 回退 → SRT，85s；产物 302 下载；真实中文转写"这是美军现役最大的直升机 CH-53E超级种马"）
- [x] 模型回环：rembg-u2net 删除 → 文件夹上传 → 删除 → zip 上传全通；isnet 经代理 URL 下载 178MB 进度采样完整、meta 写入；df3 全新安装路径（自动 venv 含 torch 约 16 分钟 + 下载 15s）
- [x] HF 缓存回收：faster-whisper-large-v3 8.7G → 2.9G（-5.8G）
- [x] 最终门禁：288 测试全过、clippy 零警告
- [x] E2E 报告：reports/wave4_e2e_report.md

### 已知限制（如实记录）
- 首次下载自动 venv 准备含 torch 约 15-20 分钟，超常见客户端超时（重试即成功）
- daemon 重启不回收模块子进程（重启前需先 stop 模块，否则端口占用）
- deep-filter 模块启动健康检查 30s 超时（torch+CUDA 首次导入慢，待查）
- max_concurrent_downloads 保留未实现；任务工作目录无自动清理
- 桌面端 GUI 无头环境无法运行时验证（编译 + 单测 + Windows 交叉编译通过）

#### Git
- commit: TBD — 待提交（工作区 61 个文件变更/新增，尚未入库）

---

## 整合包 SDK + 统一页 + 依赖栈统一 — 规划设计（2026-08-04）

### 产出（仅规划，未动代码）
- `reports/feature_audit_report.md` — 8 个并行侦察/审计代理的功能完成度审计（P0×6 / P1×13 / P2×18 + 死代码清单）
- `docs/PACK_UNIFY_PLAN.md` v2 — 设计方案 + 多代理执行计划（27 Agent / 6 波次 / 峰值并行 8），用户已确认全部 6 个决策点

### 范围
依赖栈统一（UV_CACHE_DIR + constraints + 硬链接去重 + cuda-libs 注入）、整合包 SDK（ep-pack crate + CLI）、模块/模型统一页 + 直跑 + tag + 变体单槽位 + 全限定 ID、管线修复与增强（VRAM 账本 / 设备绑定 / 导入导出 / OpenAI 兼容 LLM 节点 / 多管线并发）、ROCm/OpenVINO/DirectML 检测器、审计缺口全补。

### 执行环境
Windows PC（NVIDIA GPU + Intel NPU + iGPU 异构真机测试）+ Linux 双平台；执行时按 PACK_UNIFY_PLAN.md §9 规则与 §10 波次矩阵。

### 同期环境修复（本地，不入 git）
- 从 /server/samba/Files/AI 核对/导入 5 模块模型；faster-whisper GPU 推理修复（补 libcublas.so.12 → runtime/cuda-libs，管线 80s→15s）
- .gitignore 修正：runtime/ 整目录忽略（原仅 runtime/bin/，venvs/cuda-libs 有误导入风险）

---

## 最终统计

| 指标 | 值 |
|---|---|
| Rust 测试数 | 288（2026-08-04 WebUI E2E 强化后；含单元/集成/daemon/桌面端） |
| Clippy warnings | 0（--workspace --all-targets -D warnings） |
| Rust 源文件数 | ~70 .rs files |
| 前端源文件数 | 58 (.ts/.tsx) |
| 前端代码行数 | ~9500 行 |
| 桌面端代码行数 | ~3100 行 |
| Crate 数 | 4 (ep-core, ep-daemon, ep-desktop, ep-webui) |
| Release 构建时间 | ~3m 34s (含前端) |
| Git commits | 25+ |
| E2E 测试 | ✅ 真实媒体全流程（视频→音频→ASR→SRT 产物下载，含 CPU 回退）+ 模型上传/下载回环 + 浏览器 8 页巡测零错误 |

---

## TODO（待办）

- [ ] `build.sh` 支持 `--distro <发行版>` 参数：默认自动检测当前发行版选择包格式，用户可显式指定目标发行版
  - 当前实现：`--distro` 仅用于决定包格式（deb / rpm / PKGBUILD），未识别时只产出 tar.gz 兜底包
  - 待实现：具体发行版适配（glibc 版本约束、依赖包名差异、系统服务与安全策略等）
