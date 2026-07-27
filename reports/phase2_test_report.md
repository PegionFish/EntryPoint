# EntryPoint Phase 2 全链路测试报告

> 测试时间: 2026-07-27 ~ 2026-07-28
> 测试环境: Windows 11, RTX 5090 D (32GB VRAM), Python 3.12.13 (uv-managed)

---

## 1. 执行概要

### 波次结构（5 波，8 并行 agent）

| 波次 | 内容 | 耗时 | 结果 |
|------|------|------|------|
| Wave 0 | 环境基座：uv python 3.12 + TUNA 镜像 + env.rs 检测修复 | ~8min | ✅ |
| Wave 1+2 | **8 agents 同时并行**：5 模块 venv + 3 代码开发 | ~9min | ✅ |
| Wave 3 | 集成冒烟：cargo test + clippy + release build | ~5min | ✅ |
| Wave 4 | 全链路实机测试 + computer_use GUI + 性能基准 | ~30min | ✅ |
| Wave 5 | 报告 + 优化建议 | — | ✅ |

### 并行 Agent 清单

| Agent | 代号 | 任务 | 结果 | 耗时 |
|-------|------|------|------|------|
| A | ProcessForge | faster-whisper venv + deps | ✅ 38 包 | 5.6s |
| B | DeviceMaster | deep-filter venv + deps | ⚠️ 36 包，缺 torch | 7m10s |
| C | PipelineCrusher | paddleocr venv + deps | ✅ 75 包 | 24s |
| D | UIWeaver | qwen3-tts venv + deps | ✅ 60 包，torch CPU-only | 42s |
| E | EnvRunner | rembg venv + deps | ✅ 48 包，numba 冲突已解决 | 65s |
| F | DaemonForge2 | ep-daemon 真实模块生命周期 API | ✅ commit 6db7c61 | 8.5min |
| G | UIWeaver2 | ep-desktop 模块管理 UI 增强 | ✅ commit f9e5599 | 8.5min |
| H | PipelineForge | ep-core 真实 HTTP 模块调用 | ✅ commit 89810c4 | 8.9min |

---

## 2. 环境状态

| 项目 | 状态 | 详情 |
|------|------|------|
| uv | ✅ | v0.11.32 (pip 安装) |
| Python 3.12 | ✅ | CPython 3.12.13 (uv 管理) |
| TUNA 镜像 | ✅ | `~/.config/uv/uv.toml` |
| cargo test | ✅ | 131/131 passed |
| cargo clippy | ✅ | 0 warnings |
| cargo build --release | ✅ | 17.17s |
| RTX 5090 D 检测 | ✅ | 32607 MB, 37°C |

### 模块环境

| 模块 | venv | 依赖 | 模型 | CUDA | 状态 |
|------|------|------|------|------|------|
| faster-whisper | ✅ | ✅ 38 包 | ✅ large-v3 (3GB) | ✅ cublas64_12.dll | **运行中** |
| deep-filter | ✅ | ⚠️ 缺 torch | ❌ 未下载 | — | 降级模式 |
| paddleocr | ✅ | ✅ 75 包 | ✅ ocr_det + ocr_rec | — | 就绪 |
| qwen3-tts | ✅ | ✅ 60 包 | ✅ 1.7B-Base | ⚠️ torch CPU | 就绪 |
| rembg | ✅ | ✅ 48 包 | ✅ u2net.onnx | — | 就绪 |

---

## 3. Daemon API 测试

| 端点 | 方法 | 状态 | 响应 |
|------|------|------|------|
| `/health` | GET | ✅ 200 | `{"status":"ok","version":"0.1.0"}` |
| `/devices` | GET | ✅ 200 | RTX 5090 D (cuda:0) + CPU |
| `/modules` | GET | ✅ 200 | 5 模块，含 service_status |
| `/modules/:id/start` | POST | ✅ 200 | `{"status":"starting","port":18000}` |
| `/modules/:id/stop` | POST | ✅ 200 | `{"status":"stopped"}` |
| `/modules/:id/status` | GET | ✅ 200 | 实时状态 + uptime |

---

## 4. ASR 全链路测试（核心验证）

### 测试文件
`[Banngai] Shoushimin Series 2nd Season [01][WEB-DL][1080P_AVC_AAC].mkv` (1324 MB)

### 结果

| 指标 | 值 |
|------|-----|
| HTTP 状态 | 200 ✅ |
| 语言检测 | ja（日语）✅ |
| 音频时长 | 1368.1s（22.8 分钟） |
| 处理耗时 | 84.8s |
| **RTF** | **0.062（16x 实时）** |
| 分段数 | 285 |
| 转录示例 | `クラスはどこだっけで本当は嘘つくなよ...` ✅ |

### 小文件测试
`test_tone.wav` (160KB, 5s 正弦波) → 0.14s 处理，RTF=0.027 ✅

---

## 5. Desktop GUI 测试（computer_use）

| 测试项 | 状态 | 说明 |
|--------|------|------|
| 应用启动 | ✅ | entrypoint.exe, 1924x1247 |
| GPU 检测 | ✅ | RTX 5090 D: 4210/32607 MB, 37°C |
| 模块列表 | ✅ | 5 模块全部显示（名称/类别/状态/设备/端口） |
| 导航按钮 | ✅ | 仪表盘/模块/管线/任务/设置 5 个按钮 |
| 中文渲染 | ✅ | CJK 字体正常显示 |
| UI 元素数 | ✅ | 56 个 AX 元素全部可交互 |

---

## 6. 发现并修复的 Bug

| # | Bug | 根因 | 修复 |
|---|-----|------|------|
| 1 | env.rs 检测不到 uv | uv 装在 Python314\Scripts\，不在 PATH | 扫描 `C:\Program Files\Python*\Scripts\uv.exe` |
| 2 | env.rs 检测不到 Python 3.12 | uv 管理的 Python 不在 PATH | `uv python find 3.12` 回退 |
| 3 | module.toml 缺 start_command | 模块从未实际运行过 | 添加 `{ROOT}/runtime/venvs/<id>/Scripts/python.exe` |
| 4 | env var 双重前缀 | daemon 传 `EP_ROOT`，process.rs 又加 `EP_` → `EP_EP_ROOT` | daemon 改为传 `ROOT`，process.rs 统一加前缀 |
| 5 | CUDA DLL 缺失 | 系统未装 CUDA Toolkit | 从 faster-whisper-offline 复制 cublas64_12.dll |
| 6 | DLL 搜索路径 | venv Scripts 不在 DLL 搜索路径 | adapter.py 添加 `os.add_dll_directory()` |
| 7 | clippy 警告 | 3 个 clippy lint | `clamp()` / `derive(Default)` / `.to_string()` |

---

## 7. Git 提交记录（本次 session）

```
225f12d fix(wave-4): add DLL search path for CUDA libs in faster-whisper adapter
36a75f2 fix(wave-4): fix env var double-prefix bug + add start_command with correct ROOT/MODULE_DIR vars
104ef60 fix(wave-4): add start_command to all module.toml files with correct EP_ROOT/EP_MODULE_DIR vars
5e64370 chore(wave-3): clippy fixes + integration smoke — 131/131 tests pass
89810c4 feat(wave-2/agent-h): implement real HTTP module calls in pipeline runner
6db7c61 feat(wave-2/agent-f): implement real module lifecycle API in daemon
f9e5599 feat(wave-2/agent-g): enhance module management UI with real process control
ef3752b feat(wave-0): fix env detection — uv python find fallback + Windows Scripts scan
```

---

## 8. 优化空间分析

### 高优先级

| 优化项 | 预期收益 | 复杂度 |
|--------|----------|--------|
| **torch CUDA 版本** | qwen3-tts/deep-filter 可用 GPU 加速 | 低：`uv pip install torch --index-url https://download.pytorch.org/whl/cu121` |
| **deep-filter torch 依赖** | 降噪模块可用 | 低：requirements.txt 添加 torch |
| **ffmpeg 集成** | 管线 FFmpeg 节点可用 | 中：下载 portable ffmpeg 或 winget 安装 |
| **模型下载自动化** | 首次启动自动下载模型 | 中：env.rs + model.rs 已有框架，需接通 |

### 中优先级

| 优化项 | 预期收益 | 复杂度 |
|--------|----------|--------|
| **ASR 流式传输** | 大文件不用等全部上传 | 中：adapter 支持 chunked upload |
| **模块日志捕获** | process.rs stdout/stderr 当前被 drop | 中：tokio 后台 reader task |
| **管线编辑器 UI** | 可视化 DAG 编辑 | 高：egui_node_graph 集成 |
| **WebSocket 实时推送** | 模块状态/进度实时通知 | 中：daemon ws/ 骨架已有 |

### 低优先级

| 优化项 | 预期收益 | 复杂度 |
|--------|----------|--------|
| **ASR RTF 优化** | 当前 0.062 已极优，可尝试 int8 量化 | 低：compute_type="int8" |
| **多模块并行启动** | 同时启动多个模块 | 低：tokio::spawn 并行 |
| **模型缓存共享** | 多实例共享模型目录 | 低：config 已支持 |
| **Linux 适配** | 跨平台 | 中：路径分隔符 + 二进制检测 |

---

## 9. 性能基准总结

| 场景 | 指标 | 值 |
|------|------|-----|
| ASR (large-v3, CUDA, MKV 22.8min) | RTF | **0.062** |
| ASR (large-v3, CUDA, WAV 5s) | RTF | **0.027** |
| 模块启动（模型加载） | 耗时 | ~40s |
| Daemon API 延迟 | /health | <1ms |
| Release 构建 | 耗时 | 17.17s |
| 测试套件 | 131 tests | 2.84s |

---

## 结论

✅ **Phase 2 全链路验证通过**

- 8 个并行 agent 同时工作，9 分钟内完成 5 模块环境准备 + 3 个 crate 代码开发
- faster-whisper ASR 在 RTX 5090 D 上达到 **16x 实时**（RTF 0.062）
- Daemon REST API 完整覆盖模块生命周期
- Desktop GUI 正确显示 GPU/模块状态
- 131 个自动化测试全部通过
- 发现并修复 7 个集成 bug

⚠️ **待解决**
- torch CUDA 版本安装（qwen3-tts/deep-filter GPU 加速）
- ffmpeg 安装（管线 FFmpeg 节点）
- 模型自动下载流程接通
