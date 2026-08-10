# E2E 回归测试报告（第二轮，2026-08-10/11）

> Task #49 · 测试方式：daemon REST API 驱动（静态/API 级优先，无实机交互）
> 基线对比：上轮全量 E2E（8/8 凌晨，6/7 通过 + 缺陷 #3/#4/#5 修复复验）
> 本轮代码增量：直跑产物扩展名修复（D-7，daemon execution.rs）、模块启动 env 统一委托、W1–W4 双端 UI 重构、管线库能力补齐、令牌对齐等

## 一、环境

| 项目 | 说明 |
| --- | --- |
| daemon | `cargo build -p ep-daemon --release` 重建成功（25.8s）；PID 24776，监听 127.0.0.1:9800 |
| 日志 | `runtime\daemon-e2e-r2.out.log` / `.err.log` |
| 健康检查 | `{"status":"ok","version":"0.1.0"}` |
| 模块 | rembg / deep-filter / qwen3-tts / faster-whisper / paddleocr 五模块，全部自动拉起成功 |
| 素材 | RemBg 样图 ×2、e2e_denoise_test.wav、OCR 测试文档图、小市民系列 MKV（1.3GB，全量与 360s 截片） |
| 测试脚本 | `runtime\e2e-r2\*.ps1`（保留不清理） |

## 二、测试矩阵（10/10 通过）

### 2.1 五模块 ad hoc 直跑（POST /api/execute/single）

| # | 测试项 | 结果 | 任务 ID | 耗时 | 产物证据 |
| --- | --- | --- | --- | --- | --- |
| M1 | rembg（u2net 默认变体） | ✅ | task-20260810-160712-0000 | 5s | `output_output.png` 292,027B |
| M2 | rembg（params.model=isnet-general-use 覆盖） | ✅ | task-20260810-160717-0001 | 5s | `output_output.png` 329,872B |
| M3 | deep-filter 降噪（D-7 回归点） | ✅ | task-20260810-160725-0002 | 5s | `output_output.wav` 288,044B（**.wav 正确**） |
| M4 | qwen3-tts 合成 | ✅* | task-20260810-161425-0005 | 25.1s | 522,284B，文件头 `RIFF…WAVE` 为真 WAV；*文件名误标 `.txt`，见发现 F2 |
| M5 | faster-whisper 转写 | ✅ | task-20260810-161854-0008 | 10s | `run_output.srt` 160B，含日语（「開けた口の奥から…」） |
| M6 | paddleocr 识别 | ✅ | task-20260810-161455-0007 | 15s | `run_output.text` 622B，识别文本非空且内容正确（公式/表格/图表文字俱全） |

M2 零联网下载核验：模块日志 `Using local weights dir for 'isnet-general-use': …\models\rembg-isnet`；daemon 日志全文无 download/hub 相关行 —— **缺陷 #4（变体目录）修复持续有效**。

M5 过程记录（均为测试参数问题，非产品缺陷）：
- 首轮不带 `output_format` → json 输出在直跑退化 DAG 的 `file_output` 节点失败（与上轮口径一致，json 能力需经 `output_format` 文件产物模式，见说明 F3）；
- 带 `output_format=srt` 后用 `e2e_denoise_test.wav`（3s）→ VAD 滤除全部音频，SRT 为空（task-20260810-161450-0006）；
- 改用 MKV 提取的 60s 真实语音片段 → 通过。

### 2.2 管线（POST /api/pipelines/execute）

| # | 管线 | 结果 | 任务 ID | 耗时 | 产物证据 |
| --- | --- | --- | --- | --- | --- |
| P1 | audio-extract（全量 1.3GB MKV） | ✅ | task-20260810-162059-0009 | 10s | `output_output.m4a` 22,387,646B |
| P2 | video-to-srt（360s 截片，language=ja） | ✅ | task-20260810-162109-0010 | 110.1s | `output_output.srt` 3,824B，日语字幕（「クラスはどこだっけ…」） |

P2 节点超时配置核验：`config/pipelines/video_to_srt.toml` asr 节点 `timeout_secs = 7200` 在位（缺陷 #3 口径），心跳看门狗未误触发。

### 2.3 取消能力（缺陷 #5 回归）

| 步骤 | 结果 |
| --- | --- |
| 提交长任务（全量 MKV video-to-srt） | task-20260810-162451-0011，进入 running 后运行 30s |
| `POST /api/tasks/{id}/cancel` | 126ms 返回 `{"ok":true,"status":"cancelled"}` |
| 终态 | `cancelled`（2s 内落定） |
| 重复取消 | HTTP 409（AlreadyTerminal 语义正确） |
| 资源回收 | `/api/health` 17ms 恢复 ok；`/api/tasks` 列表延迟 1ms（无锁滞留）；取消后探测任务 task-20260810-162521-0012 3s 完成（调度闸无泄漏） |

### 2.4 WebUI 托管

| 检查项 | 结果 |
| --- | --- |
| `GET /` | 200，index.html 引用 `index-B0NguVt3.js` + `index-DbYa4MXm.css`（对应 7116e04 构建） |
| bundle 可下载 | js 1,069,268B / css 101,524B，静态盘文件写入时间 2026-08-10 23:02 |
| `/api/health` | `{"status":"ok","version":"0.1.0"}` |
| `/api/modules` | 5 模块齐备 |
| SPA 深链 `/tasks` | 200 且回源 index.html（fallback 正常） |

## 三、回归点确认

| 回归点 | 结论 |
| --- | --- |
| D-7 直跑产物扩展名（deep-filter → .wav） | ✅ 通过（M3）；同族格式 rembg → .png 亦正确（M1/M2）。跨格式能力存在误标，见 F2 |
| 变体覆盖 params.model（缺陷 #4） | ✅ 通过（M2，本地变体目录解析，零联网下载） |
| 节点超时 7200 与心跳看门狗（缺陷 #3） | ✅ 配置在位，P2 全程无误触发 |
| 运行中取消与资源回收（缺陷 #5） | ✅ 通过（取消/重复取消/健康恢复/闸回收全项） |
| 模块启动 env 统一委托 | ✅ 五模块 autostart 均经 build_module_env 拉起成功（含 EP_MODELS_ROOT 生效，见 M2） |
| WebUI bundle（7116e04） | ✅ 引用与内容一致 |

## 四、发现的问题（未改源码，证据在案）

| 编号 | 严重度 | 描述 | 证据 |
| --- | --- | --- | --- |
| F1 | 中 | **autostart 模型预检不走活跃变体**：模块停止态直跑时，`ensure_module_running` 仅检查清单 default 模型（qwen3-tts default=1.7b 未下载）→ 409「模型未就绪」，而活跃变体 0.6b 本地齐备；手动启动路径（`start_module`）正确按活跃变体预检（有单测 `start_module_model_precheck_uses_active_variant` 佐证），两路径口径不一致。绕过方式：先手动启动再直跑（本轮 M4 即如此通过） | `crates/ep-daemon/src/api/autostart.rs` ~L185（`models.iter().find(\|m\| m.default)`）；首轮 M4 提交返回 409 |
| F2 | 低 | **D-7 扩展名派生口径对跨格式能力误标**：直跑 `file_output` 的 extension 取*输入*扩展名，TTS（txt 输入/wav 输出）产物文件名 `output_output.txt` 而实际内容为 RIFF/WAVE。同族格式（图/音）不受影响；建议按 capability `output_type` 优先派生 | M4 产物头校验 `RIFF…WAVE`；`execution.rs build_direct_pipeline` D-7 段 |
| F3 | 说明 | json 输出型能力直跑须传 `output_format`（srt/text 等）走文件产物模式，否则退化 DAG 的 `file_output` 报 "no file input from upstream"。与上轮行为口径一致（上轮 whisper 即以 srt 产物通过、paddleocr 以 text 产物通过），非回归 | M5/M6 首轮与复测对照 |

## 五、结论

**10/10 通过，零阻断回归**。上轮三缺陷（#3 超时/#4 变体目录/#5 取消传播）修复在本轮代码基线上持续有效；新增量（D-7、env 统一委托、UI 重构、管线库、令牌对齐）未引入功能性回归。新发现 F1/F2 两项潜在缺陷已记录证据，建议下轮迭代修复。

## 附：收尾

- daemon（PID 24776）与五模块进程已全部停止；9800 与 18000–19000 端口确认释放，无残留 adapter 进程。
- `config/app.toml` 为本地未提交改动，按纪律未纳入本次提交。
- 测试产物与脚本保留于 `runtime/e2e-r2/`、`workspace/tasks/` 不清理。
