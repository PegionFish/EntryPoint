# 三项缺陷修复复验报告（2026-08-08）

复验对象：缺陷 #3（commit `28da240` 超时拆分）、#4（commit `f8f48ff` 变体目录）、#5（commit `1698e50` 取消传播）。
daemon 为本报告当日 release 重建（含三修复），监听 127.0.0.1:9800，日志 `runtime\daemon-retest.out.log`（runtime 不入库，仅本机留证）。任务 ID 中的日期段沿用任务注册表的 UTC 命名（task-20260807-*），实际执行于本地 2026-08-08 00:51–01:50。

---

## 一、全量门禁

| 检查 | 结果 |
| --- | --- |
| `cargo clippy --workspace --all-targets` | ✅ 零警告 |
| `cargo test --workspace --exclude ep-daemon` | ✅ 22 项全过（ep-core / ep-pack / ep-pack-cli / ep-desktop） |
| `cargo test -p ep-daemon`（主树直跑，无锁未需隔离） | ✅ 262 + 253 全过 |

合计 537 项测试 0 失败。

## 二、缺陷复验结论总览

| 缺陷 | 修复 commit | 复验结论 |
| --- | --- | --- |
| #3 超时双重职责 | `28da240` | ✅ 机制验证通过（节点声明 `timeout_secs` 后全片跑通）；⚠️ 两处配置声明缺口待补（见 §3.1） |
| #4 变体目录不可达 | `f8f48ff` | ⚠️ 部分通过：不静默下载 + 明确报错 ✅、variant 切换正路 ✅；`params.model` ad hoc 覆盖路径残余缺口（已另派修复，见 §3.2） |
| #5 取消传播 | `1698e50` | ✅ 通过 |

## 三、逐项复验明细

### 3.1 缺陷 #3：心跳看门狗与节点硬超时解耦

**复验 A（原 TOML 基线，预期失败定位）** — task-20260807-170007-0003：
`pipeline_id=video-to-srt` + inputs 覆盖 `language=ja`，1.3GB 全片 MKV。input/extract-audio completed 后
asr 节点 failed：`node 'asr' timed out after 300s (in-flight call aborted)`。
结论：修复后的四级回退链（节点 `timeout_secs` > 管线 `node_timeout_secs` > 全局
`default_node_timeout_secs` > `default_timeout_secs=300`）按设计回退到 300s——
`config/pipelines/video_to_srt.toml` 未声明任何超时放宽，长媒体管线需要补一行配置（**待收尾补**）。

**复验 B（spec 内联管线级超时）** — task-20260807-170749-0004：
spec 声明 `[pipeline] node_timeout_secs=7200` 提交。节点硬超时确实放宽（未再出现
"timed out after" 判死），但暴露第二层缺口：执行器模块 HTTP 客户端超时取**节点**
`timeout_secs`（`executor.rs` `HTTP_TIMEOUT_SECS=300` 常量缺省），管线级声明不到达此处，
两次 300s 客户端超时重试耗尽后 failed（`request timed out`）。**待收尾补**：长媒体节点需
节点级 `timeout_secs`（或收尾方评估让管线级声明透传至客户端超时）。

**复验 C（节点级超时，机制验证通过）** — task-20260807-172856-0005：
spec 中 asr 节点声明 `timeout_secs=7200`（兼管线级 7200）。17:28:56 → 17:37:47 UTC，
**8 分 51 秒 completed，4/4 节点 completed**。任务全程无看门狗误杀、无客户端超时重试。
产物 `workspace\tasks\task-20260807-172856-0005\files\asr\asr_output.srt`（22,623 字节，
311 条 cue），覆盖 00:01:47 → 00:19:53 全片，内容为真实日语对白
（如「俺がCだ」「嘘つきだって言われたことがあるの」）。上轮 failed@300.9s 的同一素材本轮跑通。

### 3.2 缺陷 #4：变体模型目录可见性（EP_MODELS_ROOT）

**ad hoc `params.model` 覆盖路径（残余缺口，已另派修复）** — task-20260807-165308-0000：
rembg 运行中（激活变体 u2net），提交 `params.model=isnet-general-use`
（本地 `models\rembg-isnet\isnet-general-use.onnx` 178MB 齐备）→ failed，
`MODEL_NOT_LOADED`（503），错误信息指向 `expected <EP_MODELS_ROOT>\rembg-isnet\...`。
错误信息中的 `<EP_MODELS_ROOT>` 字面量为 adapter 在环境变量为空时的回退文案——
子进程实际未收到该变量。根因：`f8f48ff` 仅在 `ep_core::process::build_module_env`
注入 MODELS_ROOT，而 daemon 实际拉起模块的两条路径为手写 env，均未注入：
`api/modules.rs::start_module`（手动启动）与 `api/autostart.rs::build_env_vars`
（execute/single 与管线 autostart）。仅 ep-desktop / daemon 独立模式走公共构建函数。
**无联网下载、无 ProxyError**（"不静默下载"半项生效），但本地权重未被消费。
已另派工程师修复两条 env 构建路径，收尾阶段重建二进制后补跑本项。

**不存在的模型名负例** — task-20260807-165742-0001：
提交 `params.model=birefnet-general`（本地缺失）→ failed，明确 `MODEL_NOT_LOADED`
（503）并给出获取指引；daemon 日志全程无 download/Proxy 记录。**通过**。

**variant 切换正路（端到端通过）** — task-20260807-165848-0002：
`PUT /api/models/rembg/isnet-general-use/variant` + 重启模块后不带 params.model 提交 →
**completed**（0.6s），产物 329,872 字节透明 PNG；daemon 日志仅
`active model variant switched ... needs_download=false`，无联网下载/ProxyError，
消费本地 `models\rembg-isnet`。**通过**（复验后变体已切回 u2net 默认）。

### 3.3 缺陷 #5：取消/超时传播到执行线程

task-20260807-174014-0006：提交全片 video-to-srt 后 8 秒调用
`POST /api/tasks/{id}/cancel`，API 秒回 `{"ok":true,"status":"cancelled"}`，
任务 17:40:14 → 17:40:22 UTC 即入 cancelled 终态（asr 在飞时取消）。
daemon 日志出现新式传播记录：
`task cancelled: in-flight node aborted (module HTTP connection closed); module-side inference may still finish its current request (brief worker occupation is expected)`，
随后 `engine finished after task already terminal (watchdog/cancel won the race); engine result ignored`。
本次日志**零条**旧式 "engine thread may finish in background" 记录。
faster-whisper `/health` 于取消后约 8.5 分钟恢复 ok（残留推理收尾完毕，worker 随即释放，
与修复声明的"短暂占用后恢复"语义一致）。**通过**。

## 四、复验后现场收尾

- 本次启动的 daemon（PID 20456）已停止，9800 端口释放；
- 本次拉起的 rembg（18005）/ faster-whisper（18006）模块进程已停止，端口释放。

## 五、遗留事项（收尾阶段处理）

| # | 事项 | 状态 |
| --- | --- | --- |
| 1 | `api/modules.rs` / `api/autostart.rs` env 构建补 MODELS_ROOT（#4 残余） | 已另派修复 |
| 2 | `config/pipelines/video_to_srt.toml` 补 `node_timeout_secs` / asr 节点 `timeout_secs`（#3 配置声明） | 待收尾补一行 |
| 3 | 上述两项入库后：重建 daemon → 冒烟（#4 ad hoc isnet 本地命中）→ 统一 push | 待执行 |
