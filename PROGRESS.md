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
- commit: TBD — "chore(wave-3): clippy fixes + integration smoke"
