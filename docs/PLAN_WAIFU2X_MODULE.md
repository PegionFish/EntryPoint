# PLAN_WAIFU2X_MODULE — waifu2x 模块接入执行计划

> 状态：执行中 · 负责人：主代理（可开子代理）
> 上游：https://github.com/nagadomi/nunif （waifu2x 子项目，MIT）
> 目标：新增 `waifu2x` 模块（图像/视频超分+降噪），落位主仓
> `modules/waifu2x/`，反向同步到 `/home/bob/EntryPoint_Models/EntryPoint_waifu2x`
> （独立本地 git repo）；随后完成 API 端点收尾并全链路验证端点可用。

## 0. 事实依据（2026-09-08 双侦察冻结，勿凭记忆推翻）

**平台侧**（MODULE_SPEC/ADAPTER_API/discovery 源码级核实）：
- 模块 = `modules/<id>/` + `module.toml`；Python HTTP 形态需 `adapter.py` +
  `requirements.txt`，`python_version` 必填，`start_command` 无隐式默认必须显式。
- adapter 契约三端点：`GET /health`（就绪 200 / 启动中 503，响应<1s）、
  `GET /info`、`POST /predict/{capability}`（multipart `file` + `params` JSON
  字符串）。响应 `{status:"completed", output_type, result, elapsed_seconds}`，
  **执行器只认 `output_type ∈ {file,text,json}`，`result` 必须是绝对路径**。
- 错误体 `{status:"error", error_code, message, detail}`；码表
  INVALID_CAPABILITY/INVALID_PARAMS/INVALID_INPUT/FILE_NOT_FOUND/
  MODEL_NOT_LOADED/DEVICE_ERROR/INFERENCE_ERROR/INTERNAL_ERROR。
- 环境注入：`EP_MODULE_DIR/EP_MODEL_DIR/EP_MODELS_ROOT/EP_WORKSPACE/EP_HOST/
  EP_PORT/EP_BACKEND/EP_DEVICE_INDEX/EP_MODEL_ID/EP_LOG_LEVEL`（process.rs
  build_module_env + EP_ 前缀装配）。
- 发现：仅扫 `<root>/modules/`（跟随 symlink），改目录需重启 daemon；同步
  流 = 主仓唯一事实源 → `scripts/sync-model-repos.sh` rsync 反向同步 +
  `MOD_TO_REPO` 登记表。
- venv：`uv` + `config/constraints.txt`（**torch==2.11.0**，cu130 含 sm_120
  内核，RTX 5090 D 可用）；依赖哈希 `.ep_deps_hash` 门禁。
- 范式模板：realesr（torch 超分域 manifest）、comfyui-bridge（FastAPI 写法 +
  mock 测试 + /health 503 语义 + EP-PROGRESS 进度打印）、e2e_video_to_srt
  （条件回归 + teardown 回收子进程）。

**上游侧**（nunif master @ `d23721f1b5f0a4c92c3ee1be013180bf298730c5`
浅克隆源码级核实）：
- **不可 pip 安装**（无 setup.py/pyproject.toml），必须从仓库根
  `python -m waifu2x.cli`（PYTHONPATH=仓库根）运行；master 为 playground，
  **必须 pin commit**。
- CLI 图像/视频同一入口自动分派；关键参数：`-i/-o`、`--method
  {noise,scale,noise_scale,scale4x,noise_scale4x}`、`--noise-level {0-3}`、
  `--style {art,photo,scan,art_scan}`、`--gpu <id|-1>`、`--tile-size`、
  `--batch-size`、`--tta`、`--format png|webp|jpeg`；视频侧 `--video-format
  mp4|mkv|avi`、`--video-codec libx264|h264_nvenc|hevc_nvenc`、`--crf`、
  `--preset`、`--max-fps`、`--start-time/--end-time`。
- 上游自带 Web 服务（Bottle+waitress）**仅图片、无 /health 语义、无视频**，
  不满足 adapter 契约 → 弃用，自建 FastAPI adapter。
- 权重不在 git：GitHub Release zip `waifu2x_pretrained_models_20250502.zip`
  （**417.3 MB**，内含 `swin_unet/{art,art_scan,photo}`、`cunet/art`、
  `upconv_7/{art,photo}`）；CLI 支持 `--model-dir` 显式指权重目录。
- 依赖：requirements.txt 全家（torch 单列 requirements-torch.txt，Linux 锁
  2.7.1+cu128 —— **与本平台 constraints 冲突，见 D5**）；视频走 PyAV
  （av==15.0.0 wheel 内嵌 ffmpeg，无需系统 ffmpeg）；建议 Python 3.10–3.12。
- 许可证 MIT；PyAV wheel 含 GPL ffmpeg 仅影响二进制分发，HTTP 服务自用不受限。

## 1. 架构决策（ADR）

| # | 决策 | 理由 |
|---|---|---|
| D1 | 自建 FastAPI adapter（`adapter.py`），不用上游 Bottle web | 契约不符：无 /health 503 语义、无 /info、无视频；Bottle 栈与全仓范式不一致 |
| D2 | 推理执行 = 子进程 `python -m waifu2x.cli`，`PYTHONPATH=<MODULE_DIR>/nunif` | 上游不可 pip 安装；子进程隔离崩溃/OOM、可控超时；图/视频统一入口；进度解析 stderr tqdm → `EP-PROGRESS:NN%` |
| D3 | nunif 代码获取 = **adapter 启动惰性克隆**（pin `d23721f1`，depth=1，幂等），克隆期间 `/health`=503 | post-install 钩子环境无法可靠派生 MODULE_DIR；惰性克隆自愈、归属单一文件、/health 门禁天然对接 daemon 就绪轮询 |
| D4 | 权重 = `[[models]]` 单变体 `source="url"`（417MB Release zip）→ `EP_MODEL_DIR`；adapter 解析（已解压目录 / zip 惰性解压均可）→ `--model-dir` 显式传入 | 平台统一管理下载/更新/元数据；上游 zip 为全模型一体包，不做多变体拆分（同包内 style 参数运行时选型） |
| D5 | 依赖：模块 requirements 声明上游依赖并集（去 bottle/waitress/flake8 及 web 专用），**torch/torchvision 不 pin** → 平台 constraints 解析为 2.11.0（cu130/sm_120） | nunif 的 2.7.1+cu128 与平台 constraints 直接冲突；realesr 已验证"声明不 pin"路线；5090 需要 sm_120 |
| D6 | capabilities 两个：`upscale_image`（image→file）、`upscale_video`（video→file） | 对齐 CLI 图/视频自动分派；参数冻结表见 §2 |
| D7 | 仓库流：主仓 `modules/waifu2x/` 开发并 commit → `/home/bob/EntryPoint_Models/EntryPoint_waifu2x` `git init` + 登记 `sync-model-repos.sh` → `--only waifu2x` 同步 | MODEL_REPOS.md 既定反向同步模式；满足"外部独立 repo + 本地 git"诉求 |
| D8 | 测试两层：pytest mock（假 CLI 包 + ASGITransport 进程内，永不依赖真机/torch）+ 主仓 daemon e2e 条件回归；最终 GPU 实测 | comfyui-bridge + e2e_video_to_srt 既有范式 |

## 2. capability 参数契约（冻结；S1/S2 共同遵守）

### upscale_image（input_type=image, output_type=file, max_file_size_mb=100）

| param | type | 约束 | 默认 | CLI 映射 |
|---|---|---|---|---|
| method | select | noise_scale/scale/noise/noise_scale4x/scale4x | noise_scale | `--method` |
| noise_level | integer | 0–3 | 0 | `--noise-level` |
| style | select | art/photo/art_scan | art | `--style`（art_scan→scan 别名） |
| format | select | png/webp | png | `--format` |
| tta | boolean | — | false | `--tta`（仅真时传） |
| tile_size | integer | 16–1024 | （缺省） | `--tile-size`（有值才传） |
| batch_size | integer | 1–8 | （缺省） | `--batch-size`（有值才传） |

### upscale_video（input_type=video, output_type=file, max_file_size_mb=2048）

公共参数同上（method 去掉 4x；format 固定由 video_format 决定），另加：

| param | type | 约束 | 默认 | CLI 映射 |
|---|---|---|---|---|
| video_format | select | mp4/mkv/avi | mp4 | `--video-format` |
| video_codec | select | libx264/h264_nvenc/hevc_nvenc | libx264 | `--video-codec` |
| crf | integer | 0–51 | 20 | `--crf` |
| preset | select | medium/fast/slow/p1–p7 | medium | `--preset` |
| max_fps | integer | 1–240 | 128 | `--max-fps` |
| start_time / end_time | string | HH:MM:SS | （缺省） | `--start-time/--end-time` |

**输出规则**：优先用执行器注入的 `output_path`；否则写
`<EP_WORKSPACE>/waifu2x/<uuid>.<ext>`。响应 `result` = 输出文件绝对路径。

## 3. manifest 骨架（S1 的验收基准）

```toml
[module]
id = "waifu2x"            # [a-z0-9-]
name = "waifu2x"
version = "0.1.0"
description = "waifu2x 图像/视频超分与降噪（nunif/swin_unet）"
category = "image"
genre = "upscale"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
entrypoint = "adapter.py"
start_command = "{venv_python} {MODULE_DIR}/{entrypoint}"

[compute]
backends = ["cuda", "cpu"]
default_backend = "cuda"
vram_estimate_mb = 6000
min_vram_mb = 3000

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 600   # 首次惰性克隆 + 依赖安装余量

[[models]]
id = "pretrained-20250502"
name = "waifu2x pretrained models (20250502)"
source = "url"
url = "https://github.com/nagadomi/nunif/releases/download/0.0.0/waifu2x_pretrained_models_20250502.zip"
target_dir = "waifu2x-pretrained-20250502"
size_estimate_mb = 417
default = true

[[interface.capabilities]]  # upscale_image：params 按 §2 冻结表
[[interface.capabilities]]  # upscale_video：同上
```

## 4. 波次与子代理并行规则

### 波次

| 波次 | 内容 | 并行度 |
|---|---|---|
| W0 | 双侦察（已完成）+ 本计划冻结 + commit | 主代理 |
| W1 | 三子代理并行开发（见下）+ 主代理仓库流（git init 外部 repo、登记同步表、预热克隆/venv） | 3 子代理 + 主代理 |
| W2 | 集成联调：manifest 校验、daemon 发现、真实 venv 安装、权重下载 | 主代理 |
| W3 | 验证：pytest mock → daemon 起服 curl e2e → GPU 实测超分出图 → 文档收尾 | 主代理 +（必要时）1 子代理修缺陷 |

### W1 子代理文件所有权矩阵（防冲突铁律）

| 子代理 | 独占路径 | 禁改 |
|---|---|---|
| S1 模块骨架 | `modules/waifu2x/{module.toml, requirements.txt, README.md}`、主仓 `.gitignore`（仅追加 `modules/waifu2x/nunif/` 一行） | adapter.py、tests、docs、scripts |
| S2 适配器 | `modules/waifu2x/{adapter.py, waifu2x_runner.py, tests/**}` | module.toml、requirements.txt、docs |
| S3 文档收尾 | `docs/{AUTOMATION.md, INFERENCE_API.md, ADAPTER_API.md, MODEL_REPOS.md}` | 代码、modules/、scripts/ |
| 主代理 | `scripts/sync-model-repos.sh`（登记一行）、外部 repo git init、预热克隆/venv、全部 commit | 三方独占路径（审后合并） |

### 并行开发规则（执行约束）

1. **契约先行**：本文件 §0–§3 即冻结契约；子代理不得引入契约外端点/参数/
   字段；发现契约矛盾 → 停手上报主代理裁决，不得自行分叉。
2. **文件所有权**：只写自己独占路径；需要他人文件变更 → 列入交付报告的
   "移交项"，由主代理转交。
3. **不 commit**：子代理只产出工作区变更 + 自验证据（测试输出原文）；
   主代理逐波审查、跑测、分批 commit（保证任意 commit 可独立回退）。
4. **自验门禁**：S2 交付必须附 `pytest -q` 全绿输出；S1 交付必须附
   manifest 校验通过的证据（ep-core 解析或现有工具输出）；S3 交付必须
   附文档内路径与 §0/迁移表一致性核对清单。
5. **完成定义（DoD）**：交付报告含「变更文件清单 / 自验证据 / 移交项 /
   已知风险」四节，缺一不收。

## 5. 验证计划（W3，全部通过才算端点可用）

1. **mock 层**：`pytest modules/waifu2x/tests -q` 全绿（假 CLI 包断言
   参数映射/错误码/输出路径规则；不依赖 torch/真权重/网络）。
2. **平台层**：`cargo test -p ep-daemon` 全绿；daemon 起服 →
   `GET /api/modules` 含 waifu2x 且 manifest 有效。
3. **端点层（真实模块，本机 5090）**：
   - `POST /api/modules/waifu2x/start`（canonical 路径）→ Running；
   - `GET /api/v1/capabilities` 含 waifu2x 两条能力；
   - `POST /api/files` 上传测试图 → `POST /api/v1/inference/waifu2x/upscale_image`
     → 202 task_id → `GET /api/v1/tasks/{id}` 轮询 completed → 产物 URL
     下载 PNG，尺寸为输入 2 倍（noise_scale）；
   - `POST /api/pipelines/{id}/runs`：含 waifu2x 节点的最小管线端到端；
   - `POST /api/v1/tasks/{id}/cancel` 语义正确（409 已终态 / 404 不存在）；
   - 旧路径别名抽查（/api/models/waifu2x → 200）。
4. **仓库流验证**：`sync-model-repos.sh --only waifu2x` 后外部 repo
   工作区干净、`upstream.json`/`ADAPTER.md` 生成、git log 可回溯。

## 6. 风险与回退

| 风险 | 缓解 |
|---|---|
| PyPI torch 2.11.0 Linux wheel 无 CUDA（纯 CPU 构建） | e2e 步骤 3 前置 `torch.cuda.is_available()` 探针；失败则 requirements-cuda.txt 加 `--extra-index-url .../cu130`（realesr 同款回退） |
| Release zip 经代理下载慢/失败（417MB） | 后台预下载；失败重试 2 次；`[[models]]` 支持平台级断点由下载器负责 |
| master 上游破坏性变更 | pin `d23721f1`；克隆与解压均校验 pin |
| CLI tqdm 进度解析不稳定 | 进度仅是增强信息；解析失败降级为阶段日志，不影响结果 |
| 首次克隆+装依赖超出 ready_timeout | ready_timeout_secs=600；克隆幂等可重入 |

## 7. 验收清单（DoD）

- [ ] 主仓 `modules/waifu2x/` 完整（manifest/adapter/runner/requirements/tests/README）
- [ ] pytest mock 全绿；cargo test -p ep-daemon 全绿
- [ ] 外部 repo `/home/bob/EntryPoint_Models/EntryPoint_waifu2x` 存在且与主仓同步（sync 脚本输出 + git log 证据）
- [ ] daemon 实测：模块发现→启动→健康→推理→产物闭环全通（W3 §5.3 全项）
- [ ] 文档：MODEL_REPOS 矩阵、AUTOMATION/INFERENCE_API/ADAPTER_API 路径更新完成
- [ ] 全部变更按波次逐步 commit，`git log` 可逐段回退
