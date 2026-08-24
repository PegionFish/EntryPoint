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
- [x] 测试报告: e2e_test_report.md（报告已随 2026-08-15 收尾清理删除，结论见本档）
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
- [x] 测试报告：webui_test_report.md（报告已随 2026-08-15 收尾清理删除，结论见本档）

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
- [x] E2E 报告：wave4_e2e_report.md（报告已随 2026-08-15 收尾清理删除，结论见本档）

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

## 整合包计划执行 — ✅ 完成（2026-08-04 → 08-05）

按 `docs/PACK_UNIFY_PLAN.md` v2 六波次执行完毕（27 代理 + 骨架 2 + 返工 2，峰值并行 8，worktree 隔离 + 波次门禁合并）。

### 波次与提交
- [x] 准备：Windows 门禁阻塞修复（symlink 测试 #[cfg(unix)]、TOML 反斜杠转义、build.ps1 错误过滤器）+ Node/镜像环境 + §8.3 配置预接线
- [x] Wave S：ep-pack crate 骨架 + model_id + i18n packs 命名空间 + 前端/桌面注册点（`37eebc9`/`0b69016`）
- [x] Wave 1（A1-A6）：依赖栈统一（UV_CACHE_DIR/constraints/hardlink/哈希扩展）、CUDA 库+compute.env 注入（P0-3/P0-4 前置）、包 schema+全限定 ID、包 IO+zip-slip 防护、ROCm/OpenVINO/DirectML 检测器+CPU refresh、模型 meta+变体 vram+active_models+size_bytes
- [x] Wave 2（B1-B7）：导入编排+适配报告+注册表、daemon packs 7 路由+WS、执行引擎（全局/管线闸门+queued+注册表持久化+超时取消+VRAM 预算+wait/callback）、直跑+输入上传+autostart、模块 capabilities（P0-1 根治）、模型 tags/取消/并发闸、LLM 节点+model/device schema 对齐
- [x] Wave 3（C1-C8）：统一页+直跑抽屉+packs 页、管线节点数据驱动（P0-1/P0-2 收口）、管线页 VRAM 账本/设备绑定/导入导出/任务视图、桌面核心（P0-5/P1-6/P1-7/P1-1+管线直连执行）、桌面 UI 全页面、ep-pack-cli 六命令、设置 PUT 合并+主题三端同步、i18n 全量落盘（首轮 81 键 + 二轮 395 键）+ 四份文档
- [x] Wave 4（D1-D4）：E2E 加固 19 测试（整合包全链/直跑/wait/callback/VRAM/闸门/取消+video-to-srt 条件回归）、死代码清除 −255 行、打包修复（P2-17+5 处脚本漂移+ep-pack 入包+watcher 双平台示例）、constraints.txt 定稿+迁移文档
- [x] Wave 5：异构真机验证 + API/WebUI/桌面 GUI 冒烟 + 完整双模式打包 + 本文档

### 验证结果（Windows 执行机实测）
- 最终门禁：`cargo clippy --workspace --all-targets` 零警告；`cargo test --workspace` **936 全过**；前端 build+lint 零 error
- 异构设备真机：`/api/devices` 检出 **cuda:0（RTX 5090 D 32GB 实时显存/温度）+ openvino:NPU.0（Intel AI Boost）+ openvino:GPU.0（Intel Graphics）+ cpu（注册表 ProcessorNameString 营销名「Intel(R) Core(TM) Ultra 7 270HX Plus」+ 实时利用率）**；DirectML 为空系预期双重原因：去重策略（CUDA>ROCm>OpenVINO>DirectML）+ 虚拟显示适配器黑名单过滤（OrayIddDriver 等向日葵残留，见「设备检测修复批」）
- API 冒烟：health/modules（capabilities+active_model_id）/packs/pipelines/WebUI 首页全过
- 桌面 GUI：启动渲染完整（模块单页含变体选择器/导入导出、管线编辑器、CJK 字体）；自动化点击注入对 egui/winit 事件循环受限（驱动限制，非应用缺陷，54 桌面单测覆盖）
- 打包：`build.ps1 gui` + `build.ps1 server` 全量流程（含 clippy+tests+release），产物含 ep-pack.exe + VC 运行库 + cuda-libs 存在性附带

### 已知限制（如实记录）
- workspace 级 Linux target 交叉 check 被 openssl-sys 卡死（需系统 OpenSSL+交叉 C 工具链）；Linux 编译面靠 cfg 纪律保障，真验证留 Linux 环境
- video-to-srt 真实执行/真实模块推理/真实下载链需 ffmpeg+venv 环境（D1 条件回归测试已就绪，环境满足即自动运行）
- 桌面 GUI 自动化点击注入受限（见上）
- deep-filter 模块首次启动健康检查慢（torch+CUDA 首次导入）：超时值可在 module.toml `[interface] ready_timeout_secs` 配置（默认 30s）
- 桌面端 check_updates 运行期改动重启生效（启动时读取；daemon 端热跟随）
- WS model_update toast 去重为会话级内存态，刷新页面后同一更新会再次提示

### 信息架构终稿（用户裁决，2026-08-05 执行）
- **模型=模块**同一概念：WebUI/桌面均单页「模块管理」；旧「模型」「整合包」页与导航删除（无重定向）
- 模块卡 = 模型家族 + 变体选择器（变体不独立成模型行）；激活变体以后端 `active_model_id` 为权威
- 顶部工具栏「导入模块/导出模块」：导入 = .epzip 三来源 + WS 进度；导出 = 勾选模块(变体)+管线，**每模块许可证模式二选一**（随包附带权重 bundle / 仅元数据从指定渠道下载 reference）
- 已装包管理无独立视图：pack 来源徽章菜单「卸载来源整合包」
- 随重构移除的存量 UI 入口：**已按设计文档并回统一模块页（用户裁决 2026-08-05）**——按模型本机上传/本地路径导入（MODULE_SPEC §6.3）、检查更新、删除模型（§5.1 卡内删除）、tag 列表级筛选 chips（§5.1）全部恢复；导出产物离开页面无重下入口（任务页/直跑抽屉内产物下载已覆盖，页面级入口确认移除）
- 同期修复：配置持久化相对路径分离（#48）、build.ps1 测试计数误报（#49）、log_level 动态 reload + check_updates 自动检查（P1-10/P2-1 关闭）

#### Git
- 波次合并提交序列见 `git log --oneline --grep="wave-"`；最终门禁与打包产物提交见本节后续 commit

---

## 后续收口批（2026-08-05）— ✅ 完成

> 目标：PROGRESS TODO 清零 + #50 旧入口裁决落地 + 已知限制收口。
> 方式：主代理完成 A/B 项（含 4 波提交），C 组 3 项并行子代理（C3/C4/C5）+ 主代理串行 C1/C2/C6 + 全量门禁 D。

### A — build.sh --distro 发行版适配 ✅
- [x] `build.sh` 支持 `--distro`：发行版知识表（debian/ubuntu/mint/rhel/centos/fedora/rocky/alma/ol/arch/manjaro/endeavouros 17 项：family + 最低 glibc + 运行时依赖包名）
- [x] glibc 兼容性检查：构建机 glibc ≤ 目标发行版最低 glibc 才安全，超出打印警告（建议容器/CI 构建）；`ldd --version` 自动检测
- [x] 依赖包名差异落地：deb `Depends:`（ffmpeg/python3/python3-venv）、rpm `Requires:`；未知发行版 tar.gz 兜底 + 提示补知识表
- Git: `b6ff950` — "feat(pkg): build.sh --distro 发行版适配…"

### B — #50 统一页补齐（用户裁决：按设计文档并回统一页）✅
- [x] WebUI：模型级上传（文件夹多文件/zip/tar.gz，§6.3 协议 model_id+files+paths）、本地路径导入、卡内删除模型（确认框）、卡内检查更新、tag 列表级筛选 chips（§5.1）；i18n 键落盘 zh/en
- [x] 桌面端镜像：卡内「导入模型」（rfd 选文件/目录 → AppCmd::ImportModel）+「删除模型」（danger 确认框 → AppCmd::DeleteModel）；i18n 键落盘
- Git: `b92b006`（WebUI+static 重建）、`bd1a5ed`（桌面端）

### C — 已知限制收口 ✅
- [x] C1 daemon 优雅退出回收模块子进程：`stop_all_modules`（逐 stop_module + 端口释放），axum graceful shutdown 后执行；2 测试（真实子进程回收/空载 noop）
- [x] C2 `keep_workspace=false` 任务工作目录中间文件清理：终态后清理 task_dir（保留 `files/` 产物归集目录，下载不受影响）；2 测试
- [x] C3 裁撤 `scripts/build-desktop.sh`（已被 build.sh/build.ps1 gui 覆盖，#46）；README 引用改为 build.sh gui / build.ps1 gui
- [x] C4 桌面端 check_updates 接线（#51）：设置页新增「启动时检查更新」开关 + 启动 15s 后按开关自动 CheckAllUpdates（复用既有 handler）
- [x] C5 WS model_update 前端消费（#51）：`WsModelUpdateMessage` 类型 + 全局 hook（任意页面 toast 提示更新可用 + 跳转模块页，去重）
- [x] C6 本文档勘误：并发闸（models.rs `download_gate`）与 ready_timeout_secs 配置化早已实现，已知限制剔除过时项
- Git: `9d0e55b`（C1）、`e602bf4`（C2）、`e7527c8`（C3）、`f803ba3`（C4）、`ba117fc`（C5）

### 验证
- cargo clippy --workspace --all-targets 零警告；cargo test --workspace 全过（数量见下节统计）
- 前端 tsc + vite build 通过；桌面 cargo check 通过

---

## 设备检测修复批（2026-08-05）— ✅ 完成

### 完成（3 文件 +220/−7，新增 6 测试）
- [x] `compute/directml.rs`：虚拟显示适配器黑名单扩充 —— 过滤 OrayIddDriver/Idd 等虚拟显示适配器（向日葵远程工具残留会混入 DirectML 设备列表）；原有去重策略（CUDA>ROCm>OpenVINO>DirectML）不变
- [x] `compute/cpu.rs`：Windows CPU 名称改读注册表 `ProcessorNameString`（营销型号，免子进程，替代 PROCESSOR_IDENTIFIER 的 Family/Model 格式）→ 现显示如「Intel(R) Core(TM) Ultra 7 270HX Plus」；ARM 平台加 cpuinfo 回退
- [x] `compute/openvino.rs`：Linux lspci 路径过滤 QEMU/VMware/llvmpipe 等虚拟/软件渲染设备

### 验证
- ep-core 367 测试全过；workspace 除 video_to_srt E2E 外全过

### 遗留（如实记录）
- ~~video_to_srt E2E（wait:true）卡死问题排查中~~ → 已定位根因并修复，见「video_to_srt 卡死修复批」

### Git
- commit: `7c367a6` — "fix(ep-core): 设备检测过滤虚拟显卡+CPU营销型号"

---

## video_to_srt 卡死修复批（2026-08-05）— ✅ 完成

> 目标：根治 video_to_srt E2E（wait:true）卡死 / 孤儿 adapter 占端口链路。
> 方式：并行子代理根因调查（绑定对照实验实证）+ 作用域修复 + 干净环境复跑验收。

### 根因（实证）
- Windows 绑定语义：端口被 `0.0.0.0:port` 监听时，bind `127.0.0.1:port` **仍成功**（共存语义）→ 仅探回环的 OS 探测误判空闲；而所有模块 adapter 均 bind `0.0.0.0`（uvicorn）
- 模块子进程在测试结束后不回收：E2E 以进程内 `Router::oneshot` 驱动，不走 main.rs 优雅退出路径（C1 的 stop_all_modules 只挂 Ctrl+C）；Windows `cmd /C` 壳包裹使 kill 直接子进程只杀 cmd.exe → 孤儿 adapter 监 0.0.0.0:18000
- 叠加效果：allocate 分出被占端口 → 新 adapter 绑定失败退出（exit 3）→ 健康检查打到孤儿 → 假"就绪"（寄生执行）或长超时卡死

### 完成（+4 测试）
- [x] `ep-core/src/port.rs`：`os_port_free` 双地址探测（先 `0.0.0.0` 后 `127.0.0.1`，任一失败判占用；通配监听器先 drop 再探回环避 Linux 互斥误判）；测试串行锁 + 通配预约辅助，+2 回归测试（0.0.0.0 占用形态）
- [x] `ep-core/src/process.rs`：`kill_process_tree`（Windows 系统自带 `taskkill /T /F` 树级回收，零新依赖；非 Windows 回退直接 kill），`stop_module` 树回收优先 → 直接 kill 兜底 → reap；+1 心跳法测试（cmd /C 子孙进程形态）
- [x] `ep-daemon/tests/e2e_daemon.rs`：harness teardown `stop_all_running_modules`（语义对齐 main.rs stop_all_modules：逐个 stop + 端口释放），video_to_srt 测试结尾接入；新增 `e2e_harness_teardown` 回归模块（真实长活子进程 → teardown → 无残留 + 端口释放）
- [x] `ep-daemon/src/api/autostart.rs`：3 个测试适配 OS 探测（序列改"先 allocate → 在分得端口起 mock"，断言不变）

### 验证
- ep-core 373（360 lib + 13 集成）/ ep-daemon bin 237 / e2e 228 全过；clippy ep-core+ep-daemon 零警告；`cargo check --workspace --all-targets` 干净
- 干净环境复跑 video_to_srt E2E：8.4s 真跑通过（真实拉起模块，3GB 模型热缓存 5.85s ready，4 节点产物落盘），**跑后零孤儿、18000 释放**（对照：全冷首轮含 venv 初备 1035s 亦通过；此前 7.1s"绿"经任务记录时间线证伪为寄生孤儿）

### 遗留（如实记录）
- E2E 断言 panic 路径跳过 teardown（双地址探测 + 18000-19000 宽范围兜底，可接受）
- Linux 树回收依赖 `sh -c` 单命令 exec 优化（setpgid 进程组方案已提议未实施）；Windows job object 方案已提议未实施（taskkill /T 已足够且零依赖）
- 探测 TOCTOU 已文档化；E2E 媒体为占位 fixture（链路回归），真实媒体转写回归仍在 Linux 侧（与已知限制一致）

---

## 最终统计

| 指标 | 值 |
|---|---|
| Rust 测试数 | 1170（`cargo test --workspace -- --list` 实测口径，2026-08-08 全量修复批后；历史 997/1103 为修复前/中间口径，随回归测试持续增长） |
| Clippy warnings | 0（--workspace --all-targets） |
| Rust 源文件数 | ~90 .rs files |
| 前端源文件数 | 58 (.ts/.tsx) |
| 前端代码行数 | ~14000 行 |
| 桌面端代码行数 | ~9000 行（2026-08-13 随 ep-desktop 退役删除） |
| Crate 数 | 5 (ep-core, ep-daemon, ep-webui, ep-pack, ep-pack-cli) |
| Release 构建时间 | ~4m（含前端） |
| Git commits | 60+ |
| E2E 测试 | ✅ 整合包全链/直跑/wait/callback/VRAM/闸门/取消 19 项（D1）+ 既有真实媒体全流程（Linux 侧）+ video-to-srt 条件回归 Windows 真机复跑通过（2026-08-05） |
| 异构设备 | ✅ Windows 真机：cuda:0 + openvino:NPU.0/GPU.0 + cpu 四设备实时数据 |

---

## TODO（待办）

- 无遗留 TODO（build.sh --distro 已收口；#50 已裁决并落地；已知限制按上节如实记录）

---

## 桌面端退役（Sunset）— 2026-08-13

**裁决**：退役 `crates/ep-desktop`，WebUI 为唯一 UI，产品交付 = ep-daemon + WebUI 静态资源（server 包）。
依据：第三轮双端 E2E（`reports/e2e_uiux_report_20260813.md`）WebUI 显著领先 + 9000 行 egui 重复实现 daemon 已有逻辑的持续漂移成本。
方案全文见 [docs/DESKTOP_SUNSET_PLAN.md](docs/DESKTOP_SUNSET_PLAN.md)（含已否决的 WebView 薄壳替代方案留档）。

**范围**（Wave 0 双代理审计冻结的基线）：
- 删除：`crates/ep-desktop/` 整目录、`packaging/PKGBUILD.gui`、`packaging/entrypoint.desktop`、4 个 locale 文件（desktopPages/desktopApp × zh-CN/en）
- 清理：`Cargo.toml` workspace 成员 + GUI 依赖块（eframe/egui/egui_extras/rfd）、ep-core `UiConfig`/`AppConfig.ui`/`refresh_all_devices`、i18n 命名空间、前端 `types.ts` 的 `ui` 字段
- 移植：Windows `SetErrorMode` 子进程错误弹窗抑制 → ep-daemon（server + `--run-module` 双入口）
- 体验：`start-daemon.bat` 自动开默认浏览器（`--no-browser` 可选）；`build.ps1/build.sh` gui 调用打印迁移提示非零退出
- 保留：`scheduler.rs` pub 面（`ep-core/tests/integration_compute.rs` 合法消费）；`reports/` 桌面评估类报告（历史证据）

**commit 链**：
- `d94716d` chore(sunset/B3): 旧配置含 `[ui]` 节可解析回归测试
- `0f079a1` chore(sunset/B1): SetErrorMode 移植 daemon 双入口 + cfg(windows) 测试
- `d215ebf` chore(sunset/B2): start-daemon.bat 自动开浏览器 + --no-browser
- `4facb11` chore(sunset/C1): 退役 ep-desktop crate（workspace 成员 + GUI 依赖 + lock 重生成）
- `07c438f` chore(sunset/C6): ep-core 死代码 + locale 文件 + 前端 types.ts 清理
- `94d0b43` chore(sunset/C2): build.ps1 移除 gui 模式 + 迁移提示
- `5ed806f` chore(sunset/C3): build.sh 移除 gui/macOS 分支
- `d60f16d` chore(sunset/C4): 删除 gui 打包文件
- `aa3bc6d` chore(sunset/C5): 删除 `[ui]` 配置节与文档章节
- `4053b8f` chore(sunset/C7): 文档 server-only 化（README/DESIGN/PROGRESS）
- `fb2a922` chore(sunset/C8): 历史文档 sunset 横幅 + 活跃代码零引用核验
- （最终验收记录随本节一并落盘，见 git log 顶部）

**验收**（Wave 3 门禁，2026-08-13 实测，全部 ✅）：
- [x] workspace = 5 crate；`cargo tree` 无 egui/eframe/accesskit 残留
- [x] `build.ps1 gui` → 打印迁移提示并以非零码退出（实测退出码 1）
- [x] `build.ps1 server` 全流程绿：clippy 零警告 / 测试全过 / release 编译 / 产物 = bin(ep-daemon+ep-pack+VC 运行库) + webui + config + modules + start-daemon.bat，**无 entrypoint.exe**
- [x] `cargo clippy --workspace --all-targets` 零警告；`cargo test --workspace` 全过（ep-core 466 / ep-daemon 274 / e2e 264 / ep-pack 72 / cli 33；1 次既有 flake `test_submit_queue_full_rejected` 重跑恢复，与本次改动无关）
- [x] SetErrorMode 抑制已移植（B1，server/run-module 双入口最早期 + cfg(windows) 测试在案）；实机 daemon 启动期子进程探测（deps/ffmpeg/python/uv）无系统弹窗、无崩溃
- [x] 含 `[ui]` 节的旧 config/app.toml 可被新 daemon 正常加载（B3 回归测试在案）；`GET /api/config` 响应已无 `ui` 字段
- [x] WebUI 实机冒烟（打包产物 + Edge headless + CDP）：仪表盘/模块/管线/任务/设置 5 页零控制台错误零警告；API 10 端点全 200（health/config/devices/modules/pipelines/tasks/models/packs/deps/rembg-status）
- [x] README 快速开始 = server-only 路径（双击 start-daemon.bat → 浏览器）
- [x] 用户本地 config/app.toml 值未入库，工作区原样保留

---

## Linux 真机落地 + 自包含交付（2026-08-19）

> 目标：Linux 真机（Arch rolling）完整运行验证 + 交付物升级为 ZIP 自包含包 +
> 交互式 deploy.sh（Debian/Fedora/RHEL/Arch 四族依赖与 systemd 自动管理）。
> 用户裁决：解压目录自包含（不绑定 /opt 与发行版布局）、可完整安装但不开机自启、
> 模型全部跑通、分发 ZIP 不带模型权重。
> 方式：Wave 1 四代理并行审计 → Wave 2 三代理并行实施 → Wave 3 集成门禁 →
> Wave 4 真机 E2E（含实测缺陷修复）→ Wave 5 交付收口。

### Wave 0 基线门禁 ✅（commit 9eeba97）
- Linux 全工作区编译/clippy/测试首跑：clippy 零警告；测试暴露 1 个 AddrInUse flake
  （autostart allocate→bind TOCTOU）→ 重试加固修复
- 收编在途适配改动：env.rs 跨平台/半壳 venv 自愈、build.sh Arch rolling
  VERSION_ID 未定义崩溃修复、ep-pack Windows 盘符前缀拒绝、port.rs/models.rs
  并行测试 TOCTOU 加固、.gitattributes LF 统一
- GitHub 通道打通：全局 gitconfig 失效代理（127.0.0.1:10808 无监听）仓库级覆盖
  + 远端切 SSH（HTTPS 无凭据）

### Wave 1 并行审计 ×4 ✅
- **运行时（LNX-01..10）**：P1×3——SIGTERM 未监听（systemd stop 优雅回收旁路）、
  Linux kill_process_tree 空 stub、uv 托管 Python 不在包内；P2×7（localhost 健康
  检查/resolve_root 部署布局/依赖自动装开关等）
- **打包交付**：三套安装脚本/unit 漂移清单 + deploy.sh 完整设计（子命令矩阵、
  RPM Fusion ffmpeg 论证、unit 模板、升级/回滚）
- **测试兼容**：cfg 门控分布（Linux −4 windows-only +9 unix-only）、并行 flake
  风险分级（下载闸门/执行锁/端口/环境变量）、e2e harness 评估
- **E2E 就绪**：模块需求清单、Python 3.14 不阻塞论证（uv 托管 3.12）、
  模型本地盘点、分步 recipe、风险表（PyPI 慢网为 torch 系主要成本）

### Wave 2 并行实施 ✅（三代理；漏配 worktree 隔离致共享 checkout，
显式路径清单提交应急止损，无交叉污染——此后改动型子代理强制 worktree 隔离）
- `a4df10c` build.sh：pkg_zip 主产物（zip→python zipfile→bsdtar 降级链，
  unix 权限保留）、deploy.sh 入包 fail-fast、删内嵌 install.sh、CARGO_TARGET_DIR 支持
- `85cb4dd` scripts/deploy.sh（1003 行）：9 子命令 / 20 flags；四族依赖矩阵
  （RPM Fusion free 装完整 ffmpeg + ffmpeg-free 兜底、uv pacman/astral 双路）；
  set_toml_key 合并式配置向导；unit 现场渲染（User=目录属主、EP_ROOT 显式注入、
  TimeoutStopSec=30、无 ProtectHome、**全文无 systemctl enable**）；
  firewalld/ufw 智能分支 + 回环跳过；SELinux semanage；幂等升级/软卸载/--purge；
  11 项 /tmp 自测全过（系统零变更）
- `c5344de` 运行时六修复：SIGTERM 优雅退出（server/standalone 双入口）、
  process_group(0) + 进程组级 SIGTERM→5s 宽限→SIGKILL 树回收、
  UV_PYTHON_INSTALL_DIR 入包、健康检查 127.0.0.1、resolve_root <root>/bin/exe
  布局识别、python_version 缺省口径统一；含进程组回收真子进程测试

### Wave 3 集成门禁 ✅
- ff 合并 + clippy 零警告 + **1148/1148 测试通过**
- `3869047` build.sh SIGPIPE 竞态修复（release 打包实测 exit 141：
  `ldd --version | head -1` 早断 → sed -n 1p 全量读取）
- release 构建 + ZIP/tar.gz/PKGBUILD 全产物链路打通

### Wave 4 真机 E2E ✅（部署目录 /home/bob/ep-deploy-test，systemd 服务运行，未 enable）
- **部署链路**：deploy.sh check 9 项全过 → install --yes 一次通过
  （依赖幂等跳过/配置合并写入/unit 注册 User=bob/健康自检通过/防火墙回环跳过）
- **SIGTERM 优雅回收真机实证**：systemctl stop 逐个回收运行中模块
  （rembg/faster-whisper/paddleocr）→ "Daemon shut down gracefully"，多次重启复现
- **API 冒烟**：health/devices/modules/config/pipelines/tasks/deps/v1-capabilities 全 200
- **内置 audio-extract 管线**：completed + 产物落盘
- **模块真实推理**（模型本地 5.6GB 入部署目录，venv 全部从零重建，RTX 5090 D GPU）：
  - rembg ✅ 25s venv → RGBA PNG 抠图产物
  - faster-whisper ✅ large-v3 GPU 中文转写（词级时间戳 + 概率）
  - paddleocr ✅ "Hello OCR 2026" 置信度 0.9998 + bbox（含 bcebos 权重自下载）
  - deep-filter / qwen3-tts：torch venv 重建验证中（8.5GB wheel 缓存就位），
    推理未等待收口（用户裁决跳过）
- **WebUI**：资源/SPA fallback/API 层全通；Firefox 无头渲染受 SWGL 环境限制（记录）
- **实测缺陷修复**：
  - `7003149` v1 facade JSON/Text 输出直跑必挂 → 两节点退化 DAG + 内联产物物化
    + output_url 优选（run 节点产物优先于 input）
  - `5e92c0f` UV_HTTP_TIMEOUT 30s→300s（networkx 解压中途超时拖垮整次安装实测）
  - `6935989` lspci "Non-VGA" 未分类设备误报第二块 iGPU + 设备名规范化
    （去 Intel Corporation 前缀/(rev NN) 后缀）
- **实证约定**：systemd PrivateTmp → 输入文件须在部署目录内；v1 接口强制
  workspace/uploads 前缀；active_models 变体切换（qwen3-tts 1.7b→0.6b）经
  PUT /api/config 合并生效
- **设备检测实证**（本机 = Core Ultra 7 270K Plus + RTX 5090 D）：
  cuda:0 + openvino:NPU.0 + openvino:GPU.0 + cpu；5 模块均未声明 openvino
  后端 → NPU/iGPU 暂无模块消费（遗留）

### Wave 4b 管线多端口特性（并行 worktree 实施中）
- 需求（用户场景）：视频拆轨分流 → 音频降噪→ASR→LLM 双语 SRT / 视频分支并行
  → 终端混流合一。引擎侧：ffmpeg 节点 `{output:<name>}`/`{input:<port>}`
  命名端口 + llm 文本产物物化 + video_bilingual_srt 示例管线
- WebUI 编辑器多端口手柄为后续波次

### Wave 5 交付收口
- 最终门禁：clippy 零警告 + 1150/1150 测试通过
- DEPLOYMENT.md 全量重写（ZIP 自包含模型）、README Linux 章节更新
- 最终 ZIP 产物验证与统一 push（见 git log）

---

## Wave 6 异构计算落地 + 模块独立分发（2026-08-22，进行中）

方案：`docs/HETERO_DIST_PLAN.md` v5（W0 契约冻结 `b6ab553`）。
子代理舰队 8 流并行（A-core/A-api/B/C/D/E/F/G），本节为执行快照。

### 已交付（全部已 commit，未 push）

| 流 | 内容 | 提交 |
|---|---|---|
| WS-A-core | M2 requirements_by_backend 消费 / M3 分后端 venv `<module>--<backend>`（旧布局兼容）/ M4 Vulkan 检测器（注册序 DirectML 后 CPU 前）/ M6 OPENVINO_DEVICE `{device_name}` 注入修复 | `c9555c0` |
| WS-A-api | POST /api/modules/import（zip/tar.gz 安全解包+semver 升级门禁+导入即纳管）/ GET export（内附 SHA256SUMS.txt）/ WebUI 导入对话框；ep-daemon 309+302 tests 全绿 clippy 零警告 | `47ff466` |
| WS-B | **未派出**（两轮 spawn 被截断）——rocm 依赖准备 + faster-whisper requirements_by_backend 接线，待补 | — |
| WS-C | rembg 多后端化（ORT provider 按 EP_BACKEND 分派 + requirements_by_backend(openvino)）/ onnx-matting 新模块（BiRefNet 双变体）/ ort_ep NPU 设备名归一化 | `bbe8de0` |
| WS-D | qwen3-asr 新模块：PyPI qwen-asr 0.0.6；transcribe+align 双能力（Aligner 复用 ASR 实例零重复显存）；三模型双源声明 | `c1edf18` |
| WS-E | firered-ocr 新模块（实为 Qwen3-VL-2B 微调 ~2.1B，Apache-2.0 已核） | `b3a96cb` |
| WS-F | video-upscale / video-interp 三运行时分层脚手架（torch 懒守卫/ORT-OV/ncnn subprocess，未实现分支 501）+ 引擎决策备忘录 | `8152c3c` |
| WS-G | 许可证矩阵零"待核"回填（GFPGAN 升 A、CAIN/DAIN 实有 MIT、SRMD 零许可判排除）+ MODULE_SPEC v1.3-draft（[distribution] 字段/vulkan 词表行）+ PACK_AUTHORING §10 标准压缩包分发 | `f18a79e` |

### 真机验证进展

- ✅ **E4 cuda 全片基线**：《Sound Euphonium 3》EP01（24'56"，日文主音轨）large-v3
  转写 401 段，与 CHS.ass 参考字幕时间轴命中率 **99.5%**（368/370 正文段 ±1.5s）
- ✅ **E3 冒烟**：Arrow Lake iGPU `GPU.0` OV EP 激活，Conv 计算与基准一致
- ✅ **NPU 驱动排障链**（E2 前置）：驱动更新后设备节点迁移 `/dev/accel0`→`/dev/accel/accel0`
  （新 UAPI）；bob 加入 render 组 + setfacl 即时授权；OV 2025.4.1 单 NPU 枚举裸名
  `NPU`（`NPU.0` 索引被拒）→ ort_ep 归一化补丁 + 单测更新；裸 NPU 会话激活且真实
  Conv 输出与基准一致
- ✅ **uv 硬链接缓存实证**：Windows venv 移入 runtime/venvs.win64.bak 后，
  faster-whisper Linux venv 秒级重建（ct2 4.8.1）
- 🔧 **libcublas 缺失修复**：CT2 静默落 CPU 致全片超 300s 节点窗 → nvidia-cublas-cu12
  wheel 硬链入 runtime/cuda-libs（LD_LIBRARY_PATH 注入链路真机确认）

### 恢复后增量（同日第二轮）

- ✅ **E2 openvino:NPU 平台全链**：single_device 钉 NPU.0 → `rembg--openvino` venv
  自动构建（M2/M3 消费端落地）→ OPENVINO_DEVICE 注入 → OV EP 激活（无回落）→
  宇航员图抠图前景 54.4%，与 iGPU 结果**二值差异率 0.0**
- ✅ **E3 openvino:GPU.0 平台全链**：least_memory 落 GPU.0，同一输入推理完成
- ✅ **E1 rocm 平台全链**：WS-B 补派交付（CT2 ≥4.7 Release 附 HIP wheels 的两步
  安装法 + requirements_by_backend 接线）；宿主机补装 rocm-hip-runtime/hiprand/
  hipblas/libomp 后，7900XTX 上 large-v3 ja 转写与 cuda 基线同文
- 🔧 **P1-6 缺陷修复（5f915ef）**：daemon select_module_device 走的是不带
  single_device 名称的兼容入口——`[compute].single_device` 此前静默失效，
  E2 命中属 compatible[0] 巧合；补传名称后 rocm:0 真机钉位成功
- 📌 **post-install 钩子缺口**：requirements_by_backend 只能选 pip 文件；
  CT2-HIP 需"装后覆盖 Release 轮子"，当前以手动执行 setup 脚本覆盖段桥接，
  平台侧待议 `runtime.post_install_<backend>` 或固定名脚本约定
- M4 Vulkan 检测器真机上线：/api/devices 新增 vulkan:0 / vulkan:2

### 异构矩阵记分板（G1）

| 实验 | 后端×设备 | 状态 |
|---|---|---|
| E1 | rocm × RX 7900 XTX | ✅ 平台全链（ja 转写与 cuda 同文） |
| E2 | openvino:NPU.0 × Core Ultra NPU | ✅ 抠图一致（diff=0.0） |
| E3 | openvino:GPU.0 × Arrow Lake iGPU | ✅ 前景 54.4% |
| E4 | cuda × RTX 5090 D | ✅ 全片 401 段 / 99.5% 时间轴命中 |
| E5 | 调度矩阵 | ✅ 单元+集成绿；真机 single/least_memory 双策略实测 |
| E6-E8 | SR/VFI 三运行时 | ⏳ 待 ncnn 引擎二进制落地（fetch-engine.sh 已备） |

### 收官增量（同日第三轮——TODO List 清零）

- ✅ **E8 平台全链**：video-upscale x4（320x240→1280x960）与 video-interp 2x
  （30帧@15fps→60帧@30fps）经 single_device=vulkan:0 钉位完成；ncnn 子进程
  分派 + `video-{upscale,interp}--vulkan` venv 自动构建（M2 词表扩 vulkan 后
  首个真实消费方）
- ✅ **新模块真机冒烟全过**：onnx-matting（BiRefNet cuda 软边抠图，前景53.4%）、
  qwen3-asr（0.6B ja 转写与 whisper 基线同文）、firered-ocr（"Hello OCR 2026"）
- ✅ **W3 分发闭环**：export(内附 SHA256SUMS.txt+头部哈希)→删除→import 回装
  目录一致→运行正常；同版重导入 409 拒绝；手动解压 modules/ 自动纳管
- ✅ **post-install 钩子机制**（4a4012a）：ensure_venv 安装后执行
  scripts/post-install.sh（VIRTUAL_ENV+EP_BACKEND 注入、失败 fail-fast 且哈希
  不落盘）；faster-whisper rocm 覆盖段改造为首用例
- 🔧 冒烟暴露并修复：video 变体依赖未叠基础栈（M2 单文件语义违反）、
  onnx-matting /health 503 与健康门禁互等死锁、firered-ocr 缺 torchvision、
  uv 缓存 sox sdist 条目损坏、ep-pack notes 词表测试随 M4 过时（23916cd2）
- ✅ 最终门禁：workspace **1230/1230** tests + clippy 全仓零警告

### 异构矩阵终版（G1 全绿）

| 实验 | 后端×设备 | 状态 |
|---|---|---|
| E1 | rocm × RX 7900 XTX | ✅ 平台全链 |
| E2 | openvino:NPU.0 | ✅ diff=0.0 |
| E3 | openvino:GPU.0 (iGPU) | ✅ |
| E4 | cuda × RTX 5090 D | ✅ 全片 99.5% |
| E5 | 调度矩阵 | ✅ |
| E6 | 引擎落地 | ✅ ncnn 二进制+公版模型入位 |
| E7 | OV ONNX SR | ⏸ 无权威 ONNX 直链（memo §1.2），501 诚实占位 |
| E8 | vulkan 兜底 × 三卡 | ✅ upscale+interp 平台链 |

### 追加（同日第四轮——仪表板设备归并）

- 🔧 **ROCm 键名漂移修复**：新版 rocm-smi 输出 `Card Series`/`Device Name`
  （大写 S），旧解析仅认 `Card series` → 静默退化通用名「AMD GPU 0」；
  现按忽略大小写多候选键解析（Card series→Device Name→Card model→兜底）
- ✨ **跨栈物理设备归并**（ep-core::compute::physical，纯函数）：同一物理卡
  的多栈视图折叠为一条目——匹配规则保守优先：Cpu/NPU 不参与、厂商类别须
  一致、核心名（剥圆括号尾注；方括号是 OpenVINO 家族锚点仅去字符）
  词集互含或 `AMD GPU <n>` 兜底名厂商级通配；每组每后端至多吸收一成员
  （双卡同型防误并）。调度器消费的 state.devices 保持逐栈条目不变，
  仅 /api/devices 显示层折叠并新增 `stacks` 字段（如 ["rocm","vulkan"]）；
  展示名取括号剥离后词数最多者（专有名优先于通用名）
- ✨ 前端 DeviceCard 渲染栈徽章组（主栈高亮）、管线节点设备下拉升级为
  「id + 设备名 + 多栈提示」
- 真机效果：7 条目 → 5 条目，7900 XTX 与 iGPU 各归一处且栈覆盖一目了然

### 追加（同日第五轮——模块按模型家族重组）

- ✨ **家族正名**：video-upscale→`realesr`、video-interp→`rife`、
  onnx-matting→`birefnet`；video-upscale 按家族拆为 `realesr`
  （主线+x4plus+ncnn 兜底）与 `animevideo`（v2 xsx2/xsx4，torch-only）
  两模块——平台约定「一个模块 = 一个模型家族」；rembg 为多家族工具名惯例
  保留。模型目录/激活变体键/Rust fixture 同步迁移
- 🎨 管线编排模块面板弃用 category 归堆（此前超分+插帧同压「视频」标题下），
  改为平铺家族列表（id 字母序），检索照常
- 🔧 **torch 路线首通三雷排爆**：① basicsr==1.4.2 setup.py 硬依赖已从 PyPI
  移除的 tb-nightly → requirements-torch 剔除两包，改由 post-install 钩子
  --no-deps 安装 basicsr/realesrgan + 补最小运行时依赖（scipy/lmdb/tqdm/
  addict/future/pyyaml/requests）；② torchvision>=0.17 移除
  functional_tensor → adapter 内别名 shim；③ SRVGG 架构映射错配 →
  `_srvgg_preset` 按文件名实证预设（v3=16/4、xsx2=16/2、xsx4=32/4）
- ✅ 真机冒烟：realesr torch(cuda) 全片 320x240→1280x960 首发、
  animevideo xsx2 torch 路线通过、rife vulkan 链重命名后回归
  （30帧@15fps→60帧@30fps）；workspace 1241 tests + clippy 零警告 +
  webui build/lint 过

### 追加（同日第六轮——管线暂存层，系统级 RAM 盘生命周期）

- ✨ **ep-core::staging**：任务级暂存管理器——双区布局
  `runtime/staging|/dev/shm/ep-staging/<task_id>/`（易失，RAM 优先）×
  `workspace/tasks/<id>/files/`(持久产物)；准入水位线（staging_floor_mb，
  缺省 2048MB）不足即落盘回退，tmpfs 探测经 /proc/self/mounts 判型；
  启动全量清扫孤儿；statvfs 可注入探针（7 单测覆盖落位/幂等/清扫/穿越）
- 🔌 执行器接线：send_module_request 注入 `output_path` 与
  `params.staging_dir` 指向暂存区（无 staging 时行为不变，既有测试零改动
  语义）；TaskRecord.staging_dir 随记录流转，终态**无条件**清算易失区
  （tmpfs 是全机共享内存，与 keep_workspace 解耦——该开关仅管盘上现场）
- 🐍 消费端：realesr/animevideo/rife 帧序列 mkdtemp 落位改读
  params.staging_dir（缺省回退 workspace，第三方直连兼容不变）
- 配置：[pipeline] 新增 staging_mode(auto/tmpfs/disk)/staging_floor_mb/
  staging_root 三键（serde 缺省即 auto，存量配置零迁移）
- ✅ 真机实证：帧序列驻留 /dev/shm、终态归零清算、输出尺寸正确；
  workspace **1248 tests** 全过 + clippy 零警告

### 追加（同日第七轮——产品裁决收敛 + 模块按需加载/空闲释放）

- 📐 **staging v2 收敛**（用户裁决：不为极端受限设备过度设计，OOM 即诚实
  反馈）：砍掉看门狗熔断/预留制；保留 水位准入 + 惰性预算
  （`staging_max_ram_mb` 缺省自动取 tmpfs 容量 25%、硬顶 75%）+ 启动清扫。
  显存同理不设防，CUDA OOM 文本原样透传任务错误面
- ✨ **模块按需加载 + 空闲自动释放**：`[modules] idle_timeout_secs`
  （缺省 1800，0=常驻）。触点=启动/提交/节点开始与完成回调/终态；
  回收器 30s 巡检（ep-core::module::lifecycle 纯函数决策，4 单测含未知
  基准保守豁免）；活跃守卫=TaskRecord.module_ids（排队/运行中任务引用的
  模块绝不误杀）；手动释放入口=模块详情页「立即释放」按钮
- 🔧 产物路径语义修正（暂存层回归暴露）：暂存区产物归集后记录改写为
  files/ 持久副本（源随清算即删）；盘上产物保持原路径保惰性重建语义——
  video-to-srt 全链回归由红转绿
- ⚙️ 设置页新增「模块生命周期」分区（空闲超时输入框，PUT /api/config 即时
  生效）；半写配置测试改为确定性表头内截断（字段增删不再漂移）
- ✅ 真机实证：45s 超时下 realesr 自动释放→VRAM 归还→再提交按需重载完成；
  workspace **1254 tests** 全过 + clippy 零警告

### 追加（同日第八轮——管线 JSON 分享 / cron 定时 / 层内并行与多上游扇入）

- ✨ **层内真并行**（runner 重构）：拓扑分层内节点 JoinSet 并发驱动，
  fail-fast（首个失败/取消 abort 其余在飞兄弟，语义对齐旧串行「失败即停层」；
  取消边界契约保持——首节点 failed(cancelled)、下游 Skipped）。
  共享 PipelineTask 不跨线程可变：在飞只读上游、成败按完成序统一落账
- ✨ **ffmpeg 多上游定向占位符** `{input.<node_id>}`：扇入合并场景定向引用
  各上游产物（`{input}` 首文件语义向后兼容）；collect_upstream_artifacts
  增带源变体；纯函数单测覆盖命中/未命中/未闭合
- ✨ **管线 JSON 导入/导出/分享**：GET /pipelines/{id}/export 输出自包含
  信封（format=entrypoint-pipeline/v1+spec，Content-Disposition 附件名）；
  POST /pipelines/import 接受信封或裸 spec，id 冲突自动 `-importedN`
  去重，导入前执行层全量校验——导出文件即分享载体
- ✨ **cron 定时调度**：零依赖五段解析器（ep-core::cron：*/n、范围、列表、
  vixie 日周 OR 语义、7=周日，6 单测）；runtime/schedules.json 独立注册表
  （与编辑器回写隔离）；PUT/GET/DELETE /pipelines/{id}/schedule；
  巡检循环 30s 求触发窗口，last_checked 水位线持久化（重启不补跑不双跑，
  停用期间推进水位防补喷）；活跃管线豁免 + 提交模板 inputs/params
- 🔧 内置 audio-extract 修 FLAC→m4a 不兼容（copy→aac 192k）
- ✅ **真机实证三连**：① 导出→导入→去重闭环；② cron `* * * * *` 每分钟
  准点触发→修参后全片提取完成→DELETE 注销；③ 扇出/扇入演示管线
  （negate∥hflip→hstack）640x240 全绿 + 并行度实锤：单分支 3.25s vs
  双分支并行+合并 3.46s（串行应 ~6.5s）
- 门禁：workspace **1271 tests** 全过 + clippy 零警告 + webui build 过

### 追加（同日第九轮——管线页 UI：导出/导入分享/cron 窗口）

- 🎨 工具栏新增 导出（下载自包含分享 JSON）/ 分享导入（选文件→服务端建管
  →自动选入编辑器）/ 定时（cron 对话框）三钮；无 currentId 时禁用导出与定时
- 🎨 ScheduleDialog：cron 输入+启用开关+最近触发任务号展示；保存/移除走
  /schedule API；api/client.ts 增 exportPipelineUrl/importPipelineShare/
  getSchedule/putSchedule/deleteSchedule；i18n zh/en 全键补齐
- ✅ 真机烟雾：schedule PUT 非法表达式本地化报错、PUT/GET/DELETE 回环、
  导出附件名 fanout-demo.pipeline.json；演示样例管线 fanout-demo 入库
- 门禁：workspace **1271 tests** 全过 + clippy 零警告 + tsc/build 过

### 待办（恢复工作时的接续点）

1. daemon 新二进制重启未完成（暂停时中断）——重启后 E2 走平台全链：
   start rembg → `venvs/rembg--openvino` 自动构建（M2 消费）→ u2net 推理核验 providers=NPU
2. E1/E6/E7/E8 实验；WS-B 补派
3. W2 集成清单：daemon 四条 venv 准备路径切分后端 API、`{venv_python}` 占位符
   分后端口径、apiModules.* i18n 键、全局 constraints 对 onnxruntime-* 发行名豁免、
   deps 扫描 `<id>--<backend>` 目录展示后缀
4. 契约反馈积压：多输入能力声明形态（align audio+text）、params array 类型、
   EP_BACKEND 未知值 fail-loud 入规范、ncnn 多文件权重/zip 自动解压支持、
   导入大小上限取值统一（模块包 1GiB vs 整合包 64GiB）
