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
