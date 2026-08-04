# EntryPoint 整合包 SDK + 统一页 + 依赖栈统一 + 全面补全 — 设计方案与执行计划

> 版本：v2（已确认） | 日期：2026-08-04
>
> 依据：8 个并行侦察/审计代理报告（汇总见 `reports/feature_audit_report.md`，下文以 P0-x/P1-x/P2-x 引用）
>
> 状态：**用户已确认全部决策点（§14），执行环境为 Windows PC（§15）**

---

## 1. 背景与目标

### 1.1 用户目标（本次会话逐条确认）

1. **依赖栈统一（方案 A）**：统一 torch/CUDA 版本 + uv 硬链接去重，降低磁盘占用（现状：deep-filter venv 4.8G torch、uv cache 6.7G 与 venvs 6.8G 双份、硬链接未生效）
2. **模型整合包 SDK**：用户可把自己导入的模型 + 配置打包发布（GitHub 等平台分发），他人导入后自动适配平台
3. **模块页与模型页整合**：模型即模块（可运行单元），用户 tag 自组织；模型管理页并入
4. **单模型直跑**：点模型卡片 → 配置参数 → 直接执行，免开管线
5. **版本简化**：同一模型族不并列多版本；变体是安装/配置时的选择，单槽位
6. **管线可用化 + 增强**：修复拖拽编排（审计确认两处 P0 契约断裂）；编辑时实时计算 VRAM 占用；节点可 pin 变体、可绑定设备；管线可导出/导入分发
7. **全限定模型 ID**：APK 风格 `<publisher>.<vendor>.<model>@<variant>`
8. **异构计算打基础**：ROCm/OpenVINO 基础支持；设备绑定；CUDA 为默认但 schema 全面预留 backend 维度
9. **全面补全审计发现的未实现功能**（P0×6 / P1×13 / P2×18，详见审计报告）

### 1.2 排除

- 多实例并发跑同一模块的不同变体（单槽位语义，见 §5.2）
- OpenVINO IR 自动转换、跨设备自动切分调度（schema 预留，不实现）
- ROCm/OpenVINO 真机验证（本机无硬件，做 best-effort 实现 + fixture 测试）
- 模型文件入 git；本次规划不动现有已装模型

---

## 2. 现状要点（决策依据）

- 引擎内核、HTTP API 面、上传防护、下载进度链、i18n 门禁均**真实可用**，可直接依赖（审计报告 §6）
- 管线断裂根因：`/api/modules` 不暴露 manifest capabilities → 前端硬编码猜测（P0-1/P2-3 同根因，修复杠杆高）
- 调度器四策略完整但未接线（P1-1）；`manifest.compute.env` 已解析未接线；`cleanup_hf_cache` pub 无调用方——三者本次全部接线
- uv 改造收口点唯一：`ensure_venv` 的两处 uv 调用（env.rs:369/404）；依赖哈希只覆盖 requirements.txt 需扩展（P2-18）
- 模型 Ready 判定只看目录非空、meta.source 为自由字符串 → 整合包 `source="pack"` 零障碍
- torch 是 deep-filter 与 qwen3-tts 的硬依赖，不可裁剪；去重靠统一版本 + 硬链接

---

## 3. 设计 A：依赖栈统一

### 3.1 机制

| 项 | 设计 |
|---|---|
| uv 缓存入应用根 | 新增 `[python].uv_cache_dir`（默认 `runtime/.uv-cache`），EnvManager 对两处 uv 调用注入 `UV_CACHE_DIR`。缓存与 venv 同盘 → 硬链接生效，跨 venv 同版本包只占一份物理空间；且缓存随应用目录可移植 |
| 全局 constraints | 新增 `[python].constraints`（默认 `config/constraints.txt`，不存在则跳过）；`uv pip install` 追加 `-c <file>`。默认文件锁 torch 全家桶版本（与 deep-filter venv 现行 2.13.0+cu130 对齐），保证多模块解析到同一版本 → 硬链接去重生效 |
| link-mode | 同一调用点显式 `--link-mode hardlink`（跨文件系统自动回退 copy，uv 内建） |
| 哈希扩展 | `.ep_deps_hash` 哈希输入 = requirements.txt 字节 + constraints 文件字节（若存在）+ link-mode 版本号；constraints 变更即触发重装 |
| 共享 CUDA 库 | 新增 `[compute].cuda_libs_dir`（默认 `runtime/cuda-libs`，已含 libcublas.so.12）。start_module 注入 `LD_LIBRARY_PATH=<cuda_libs_dir>[:继承值]`（process.rs env 段）；`check_torch_cuda` 检测同路径注入（修 P1 误报）；scripts/start.sh 与 packaging 同步指向（P2-17） |
| compute.env 接线 | start_module 按当前 backend 读取 `manifest.compute.env.<backend>` 表，替换 `{device_index}` 后注入（CUDA_VISIBLE_DEVICES 等多卡隔离立即生效） |
| 代理注入补全 | AppState 构造 ProcessManager 时 `with_network_env(cfg.network)`（修 P1-8） |

### 3.2 预期收益（本机实测基线）

deep-filter 4.8G + 未来 qwen3-tts ~4G → 统一版本硬链接后物理 ~4G 一份 + 各模块薄壳 ~1G；uv cache 与 venvs 不再双份（cache 成为硬链接源，du 口径下降 ~6G）。

---

## 4. 设计 B：模型整合包 SDK（ep-pack）

### 4.1 定位

整合包 = **模型 + 管线 + 运行约束** 的可分发单元。包是"应用"，管线是"配置"，模型权重可随包（bundle）或仅引用（reference，导入时按模块声明下载）。分发渠道：GitHub Release / 任意 URL / 本地路径 / 浏览器上传。

### 4.2 包格式（冻结契约）

**清单 `ep-pack.toml`**（serde 惯例对齐 module.toml：lowercase 枚举、Option 可选、default 缺省）：

```toml
[pack]
id = "pigeonfish.subtitle-kit"     # <publisher>.<pack-name>，全局唯一键
version = "1.0.0"                  # semver（引入正式版本比较，替代现有纯字符串）
name = "字幕制作整合包"
description = "视频转字幕 + 降噪一体化"
authors = ["pigeonfish"]
license = "MIT"
homepage = "https://github.com/pigeonfish/subtitle-kit"
min_ep_version = "0.1.0"
tags = ["字幕", "视频"]

[compute]
backends = ["cuda", "cpu"]         # 包声明可利用的后端（导入时与本机设备比对）
notes = { rocm = "需 torch-rocm wheel" }   # 每后端运行备注（自由文本，展示用）

[[models]]
qualified_id = "ep.systran.faster-whisper"   # 全限定 ID（§4.3）
variant = "large-v3"
mode = "reference"                 # reference=仅描述符 | bundle=权重随包
tags = ["字幕"]

[[pipelines]]
file = "pipelines/video_to_srt.toml"         # 包内相对路径
```

**归档布局**（`.epzip` = zip）：

```
<pack-id>-<version>.epzip
├── ep-pack.toml
├── CHECKSUMS.toml        # 包内所有文件 sha256（导入先验后落盘）
├── models/<target_dir>/  # bundle 模式权重（可选）
└── pipelines/*.toml      # 管线定义（可选）
```

### 4.3 全限定模型 ID（冻结契约）

- 语法：`<publisher>.<vendor>.<model>`，各段 `^[a-z0-9][a-z0-9-]*$`；变体独立维度 `@<variant>`
- 保留发布者 `ep`（仓库内置模块）；现有 manifest 简单 id 自动归一为 `ep.<vendor>.<model>`（向后兼容层）
- `[[models]]` 增加 `qualified_id` 字段 + 变体级 `vram_estimate_mb`（模块级兜底）
- `.ep_meta.json` 增加：`qualified_id`、`tags: []`、`pack_id`（来源包，可空）；`source` 增加取值 `"pack"`
- 落点：ep-core 新增 `model_id.rs`（解析/校验/归一），manifest/meta/pack/管线节点统一消费

### 4.4 导入流程

```
来源(local/url/upload) → 暂存 .pack-staging/<id>/
 → 解包（复用 upload.rs 防 zip-slip 模式：路径清洗 + symlink 逃逸防护，提取为共享工具）
 → CHECKSUMS.toml 全量校验 → ep-pack.toml 校验（schema + 模块存在性 + 平台/后端适配）
 → models: bundle → 落位 models/<target_dir>（TOCTOU 双检，绝不合并进已有目录）+ 写 meta(source=pack)
          reference → 按模块 manifest 解析下载源，后台下载（复用 DownloadHandle 进度设施）
 → pipelines: 校验后落 config/pipelines/（重名 → 冲突报告让用户选覆盖/改名）
 → 注册 runtime/packs/<pack-id>.json（已装包注册表：版本/内容清单/安装时间）
 → cleanup_hf_cache 清理冗余副本（接线现成 pub 钩子）
 → WS type=pack_import 进度事件
```

**安全模型**：包不携带可执行代码（模块按 id 引用，必须已安装——缺模块报适配报告而非静默失败）；路径清洗；checksum 先行；大小上限复用上传约束。

### 4.5 导出/发布

`POST /api/packs/build`：选择模型（按 tag 圈选或逐个勾）+ 管线 → 生成 ep-pack.toml（bundle/reference 逐模型可选）→ 打包含 CHECKSUMS → 下载 `.epzip`。用户自行上传 GitHub Release。**"tag 组装"闭环**：统一页打 tag → 打包向导按 tag 一键圈选。

### 4.6 平台自动适配

- **权重层**：onnx/safetensors/CTranslate2 格式天然跨平台，无需处理
- **二进制层**：包内若有 native 资产，用 `<os>-<arch>` key 表（如 `linux-x86_64`/`windows-x86_64`），导入器按当前平台选择——**同时补上 manifest `runtime.binaries` 从未实现的按 key 选择逻辑**（审计确认消费端只取第一项）
- **依赖层**：venv 本就按平台重建（既有设计）；整合包 `[compute].notes` 给出后端依赖提示；`[runtime] requirements_by_backend` 映射字段**本次只冻结 schema，不实现**（留白）
- **适配报告**：导入前输出逐模型"将运行于 cuda:0 / CPU 保底 / 不支持（原因）"

### 4.7 SDK 表面

- **ep-pack crate**（库）：manifest 类型/校验/build/extract/校验和/导入编排。ep-daemon 与 CLI 共用
- **ep-pack-cli**（独立二进制）：`ep-pack new / validate / build / import / info / export`，离线打包作者工具
- **daemon API**（§8 契约表）+ WebUI/桌面端整合包管理区

---

## 5. 设计 C：统一页 + 直跑 + tag + 版本简化

### 5.1 统一页（/models 路由，models.tsx 重构）

- 卡片 = (模块, 激活模型) 投影：模型名 / qualified_id / 模块运行状态 / 模型就绪状态 / 变体 / tag / VRAM 估算
- 卡内操作：启动·停止·日志（抽屉）·**运行**（直跑）·配置（变体切换/下载源）·删除
- 模块详情（能力/参数 schema）= 卡片展开抽屉；`/modules/:id` 保留深链接
- tag：chips 筛选 + 卡片编辑；存 meta，随整合包流转
- 无模型 native 模块渲染为服务卡（兜底）
- 桌面端同页镜像（egui 卡片列表）

### 5.2 版本单槽位语义（冻结）

- 每模块同一时间一个激活变体；激活状态存 `config/app.toml [active_models] <module_id> = <model_id>`（daemon 启动模块优先读它，回退 manifest `default=true`——修"死板取 default"）
- 变体切换：选变体 → 缺则下载 → 更新 active_models → 重启模块生效
- 管线 pin 的变体与激活不一致时：**MVP 报错 + 一键切换引导，不做静默热切换**（避免执行中重启的复杂交互）

### 5.3 单模型直跑（冻结契约）

- 交互：卡片"运行" → 抽屉选 capability（来自 manifest）→ 参数表单按 `CapabilityDecl.params` schema 渲染（type/default/min/max/enum）→ 输入文件（本地路径或浏览器上传到 workspace/uploads）→ 执行 → 进度（WS）→ 结果预览/下载
- 后端：`POST /api/execute/single` → 校验模块+capability → **模块未运行则自动拉起并等健康**（修 P1-2 的直跑侧；管线侧同样受益）→ 内部编译为退化三节点 DAG（file_input→module→file_output）提交现有执行器 → 任务/产物/WS 全套复用
- 桌面端：直跑走 ep-core 直连同款逻辑（AppCmd 分支）

---

## 6. 设计 D：管线增强

### 6.1 断裂修复（审计 P0-1/P0-2/P1-16/P2-3）

- `/api/modules` ModuleResponse 增加 `capabilities`（manifest 原样序列化：name/params schema/input/output）
- 前端删除硬编码 capability 映射，全部数据驱动；ffmpeg args 改数组化编辑 + 增加 output_extension 参数；示例模板改用真实模块；`file_input.pattern`/`file_output.overwrite` 等后端不读的参数直接从 UI 移除（或实现——取移除，保持面小）

### 6.2 节点 schema 扩展（冻结，向后兼容）

```json
{
  "id": "asr", "kind": "module", "module_id": "faster-whisper",
  "capability": "transcribe",
  "model": "ep.systran.faster-whisper@medium",
  "device": "auto",
  "params": { "beam_size": 5 }
}
```

- `model`：变体 pin（缺省 = 跟随激活变体）；执行前校验（§5.2）
- `device`：`"auto"` | `"cuda:0"` | `"rocm:1"` | `"openvino:GPU.0"`……**软约束**：导入/加载时本机无此设备 → 警告 + 回退 auto，不硬失败
- WS progress 消息增加 `task_id`（修 P2-7 并发串染 + 执行锁死）

### 6.3 VRAM 预算（编辑器实时计算）

- 数据：节点取 pin 变体的 `vram_estimate_mb`（变体级，模块级兜底）；设备容量/占用取 `/api/devices`
- 算法：DAG 分层 → 每层 Σ 绑定该层且 device 匹配/auto 的节点 vram → 取每设备峰值
- 呈现：画布侧栏"每设备账本"（cuda:0 已用/管线预算/总量进度条），超限红色警示 + 建议（换小变体/改绑设备/停掉某模块）；`allow_overcommit` 决定是否放行执行
- auto 节点显示为"未分配"池，提示将由调度器按 least_memory 落位

### 6.4 管线分发

- 导出：`GET /api/pipelines/{id}/export` → 管线 TOML（含 model pin/device/params）+ 头部注释形式依赖清单（引用了哪些 qualified_id@variant）
- 导入：上传 → 校验 → 依赖解析（缺模型/变体 → 列表提示去统一页下载/切换）→ 注册
- 管线可单独流通，也可被打进整合包（§4.2 `[[pipelines]]`）

### 6.5 管线即 API：外部自动化集成

现有 `POST /api/pipelines/execute`（inputs 覆盖 + 202 异步 + 任务/产物链路）已满足外部服务调用，本次补齐无人值守三件套：

| 增强 | 契约 | 场景 |
|---|---|---|
| **模块自动拉起** | execute/single 提交时，引用模块未运行 → 自动启动并等健康（超时计入任务错误） | watcher/定时任务无人点启动（修 P1-2 的两个消费面） |
| **同步模式** | execute 请求可选 `wait: true` → 阻塞至终态，响应直接带 status + artifacts 清单（内部超时上限取 pipeline 超时配置） | 简单脚本不想轮询 |
| **完成回调** | 可选 `callback_url` → 终态时 POST `{task_id, status, artifacts}`（best-effort，失败仅 warn） | watcher/业务系统事件驱动 |

边界声明（写入 CONFIG_REFERENCE）：输入为服务器本地路径，跨机器先上传/挂载；API 无认证仅 IP 过滤，公网暴露前需认证机制（后续工作，本期不做）；`max_parallel` 闸门修复后并发提交自动排队。产品内建触发器（文件夹监控/cron）列为后续方向，本期由外部服务承担。

### 6.6 节点开发模型

| 节点种类 | 开发方式 | 本期动作 |
|---|---|---|
| **module 节点**（扩展正路） | 写模块：module.toml 声明 capability + 参数 schema，adapter.py 暴露 `/predict/<capability>` → 自动进入编辑器面板/可拖可连可执行。新节点=新模块，零引擎改动 | 文档化（PIPELINE_SPEC 增"节点开发指南"章） |
| **builtin 节点**（引擎内建） | 现状为 executor.rs match 硬编码，开发需同改引擎+前端+桥接三处 | 注册表重构**本期不做**（决策点 5 已定：全面文档化）；在 PIPELINE_SPEC 增"节点开发指南"，完整说明 builtin 添加路径（executor match 分支 + dag 校验 + 前端 BUILTIN_DEFS + 桥接四处清单）与模块节点路径 |

### 6.7 LLM 节点（external_api 改造，决策点 4）

保留 external_api 的管线位置，但功能**限定为接入 OpenAI 兼容 LLM 端点**（chat/completions 单一形状），简化代码与 UI：

- 节点类型更名 `llm`（builtin；桥接层现拒绝 external_api，无存量兼容负担，旧名保留为 alias）
- 参数（前端表单 + TOML 一致）：`base_url`（如 https://api.openai.com/v1 或本地服务）、`model`、`api_key_env`（**存环境变量名而非明文密钥**，如 `OPENAI_API_KEY`，执行时读取）、`system_prompt`、`temperature`、`max_tokens`、`output_format: text|json`
- I/O：input_type=text（上游接 ASR 的 json→文本或任意文本），output_type=text；prompt 模板支持 `{input}` 占位
- 实现：executor.rs 内置 HTTP 客户端（no_proxy 豁免规则沿用模块调用模式），失败语义与模块节点一致（重试 1 次 + 超时受 §6 节点超时管辖）；api_key 缺失/非 2xx 走 i18n 错误文案
- 典型用途：翻译、摘要、字幕润色——补齐 README 宣称而从未实现的"翻译"环节

### 6.8 多管线并发模型

多条已配置管线可同时存在、同时执行（execute 每次生成独立 task，注册表按 task_id 建键）；`max_parallel` 修复提供全局公平队列。管线维度运维面补齐：

| 项 | 契约 |
|---|---|
| **管线级任务视图** | `GET /api/pipelines/{id}/tasks?status=&limit=` → 该管线执行历史/在跑任务（替代坏掉的 `/{id}/status`，修 P1-5） |
| **管线级并发上限** | 管线 TOML `[pipeline] max_instances`（缺省跟随全局 max_parallel）；全局闸门 + 每管线 semaphore 两级；GPU 重管线可锁 1 防显存打架 |
| **排队可见性** | TaskStatus 增加 `queued`（全局或管线闸门等待）；任务列表/管线任务视图可见队列位置 |
| **任务↔管线身份** | TaskRecord 持久化 `pipeline_id`（现状仅 name）；WS progress 携带 pipeline_id + task_id（§6.2） |
| **变体冲突交叉** | 并发管线 pin 同模块不同变体 → 后到任务在模块节点前显式报错 + 切换指引（§5.2 MVP，不静默重启）；文案入 i18n |

---

## 7. 设计 E：异构计算基础

| 项 | 本次落地 | 留白 |
|---|---|---|
| 后端抽象 | ComputeBackend/DeviceId 已备；CUDA 默认 | — |
| ROCm/OpenVINO/DirectML 检测器 | best-effort 实现（rocm-smi/xpu-smi/OpenVINO 运行时查询 + 畸形输出容错 + fixture 单测），注册进 all_detectors。**用户 Windows PC 具备 NVIDIA GPU + Intel NPU + iGPU，CUDA/OpenVINO/DirectML 检测与设备绑定可在该真机验证**；Windows 下 `intel-npu-smi`/`xpu-smi` 探测路径一并实现 | ROCm 无 AMD 硬件，仅 fixture |
| CPU refresh | 补实现（/proc/stat + /proc/meminfo 差分） | — |
| 调度器接线 | daemon/desktop 设备选择改走 ComputeScheduler（least_memory + allow_overcommit + backends 过滤），修桌面端"不看 manifest backends" | 跨设备自动切分 |
| 管线设备绑定 | §6.2 软约束 + VRAM 每设备账本 | 绑定冲突自动重排 |
| 后端相关依赖 | schema 冻结：`[runtime] requirements_by_backend`、venv 命名 `<module>--<backend>` 演进方向写入 MODULE_SPEC | 实现 |
| 包级适配 | `[compute]` 段 + 导入适配报告（§4.6） | — |

原则：**本次新增的所有 schema 一律带 backend 维度且带默认值**，后续加后端零格式破坏。

---

## 8. API / WS / 配置契约总表（冻结）

### 8.1 新增 API

| 方法+路径 | 请求 | 响应 |
|---|---|---|
| GET /api/packs | — | 已装包列表（注册表） |
| POST /api/packs/import | `{source:"local",path}` \| `{source:"url",url}` | 202 `{pack_id}`，进度走 WS |
| POST /api/packs/upload | multipart `.epzip` | 202 同上 |
| GET /api/packs/{id} | — | 详情（内容清单/适配报告） |
| DELETE /api/packs/{id} | `?keep_models=true` | 卸载（模型可选保留） |
| POST /api/packs/build | `{models:[qualified_id@variant], pipelines:[id], bundle:[qualified_id], tags?:[tag]}` | 202 → 构建完成可下载 |
| GET /api/packs/{id}/export | — | `.epzip` 流式下载 |
| GET /api/pipelines/{id}/tasks | `?status=&limit=` | 该管线任务列表（含 queued/队列位置，§6.8） |
| POST /api/pipelines/vram-budget | `{spec}` | 每设备 VRAM 预算分解（WebUI 编辑器消费；桌面端直连 ep-core 同款 helper，§6.3） |
| POST /api/execute/single | `{module_id, capability, params, input_path}` | 202 `{task_id}` |
| POST /api/upload/input | multipart 单文件 | `{path}`（workspace/uploads 暂存） |
| PUT /api/models/{m}/{mid}/tags | `{tags:[]}` | 200 |
| POST /api/models/{m}/{mid}/cancel-download | — | 200/409 |
| PUT /api/models/{m}/{mid}/variant | `{model_id}` | 200（触发下载检查 + 重启提示） |

### 8.2 修改 API

- `ModuleResponse` +`capabilities: CapabilityDecl[]`（P0-1 根治）
- `POST /api/pipelines/execute` 请求可选 +`wait: bool`、`callback_url: string`（§6.5）；提交路径接入模块自动拉起
- `PUT /api/config` 改**深度合并语义**（缺省字段保留原值，修 P1-9）；重启敏感项返回 `requires_restart: true` 标记
- WS `progress` +`task_id`；新增 `pack_import` 消息类型

### 8.3 配置新增（config/app.toml）

```toml
[python]
uv_cache_dir = "runtime/.uv-cache"
constraints = "config/constraints.txt"
[compute]
cuda_libs_dir = "runtime/cuda-libs"
[packs]
staging_dir = ".pack-staging"
[active_models]            # module_id → model_id（变体单槽位）
```

---

## 9. 多代理并行开发规则

### 规则 1：文件独占

同一波次内每个 Agent 拥有排他写权限（见 §10 所有权矩阵）。**只读不受限，越界写禁止**；需要改他人文件时在交付物中列为"仲裁请求"，由编排者统一执行。

### 规则 2：契约先行

§8 契约表 + §4.2 包格式 + §6.2 节点 schema 在波次开始前冻结。下游只消费不修改；契约缺陷上报编排者，经确认才修订并广播。

### 规则 3：骨架先行

Wave S 骨架代理预注册全部路由/类型/stub/导航项/i18n 命名空间（新命名空间 `packs`，zh/en 双份空表 + NAMESPACES + include 表 + 前端 index.ts），后续代理只在自己文件填实现——消灭注册点同文件冲突。契约要求但他人文件里的函数（如 `execution::submit_direct`）由骨架打成 `unimplemented!` stub，签名冻结。

### 规则 4：隔离工作树

每个 Agent 在独立 git worktree（`isolation: "worktree"`）工作。编排者在波次门禁统一合并，Agent 不自行 merge/rebase。

### 规则 5：波次门禁

波内全并行；波末编排者统一执行：`cargo clippy --workspace --all-targets` + `cargo test --workspace` + 前端 `npm run build` + i18n 键集门禁。全绿才开下一波；失败由编排者定位到责任 Agent 返工（返工不跨波并行）。

### 规则 6：同树容忍

合并窗口内来自非所有权文件的编译错误**忽略、不代修**，记录交门禁仲裁。每个 Agent 只保证"自己的文件 + 契约冻结的接口"范围内正确。

### 规则 7：验证内建

每个 Agent 返回前必须跑通自己 crate/范围的 `cargo test -p <crate>`（或前端 oxlint+build），并在交付物附测试清单。新代码必须带测试（惯例：模块内联 tests + tempdir 集成；HTTP 层 Router::oneshot）。

### 规则 8：i18n 仲裁

`i18n/locales/**` 为编排者独占。各 Agent 在交付物附"键需求清单"（命名空间.键 + zh/en 文案），由 Wave 3 的 DocsI18n Agent 统一落盘，键集门禁自动把关。

---

## 10. 执行计划（文件所有权矩阵 + 波次）

### Wave S — 骨架（2 Agent 并行）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **S1 RustSkeleton** | crates/ep-pack/（新建 crate 骨架）、ep-core/src/model_id.rs（stub）、ep-core/src/lib.rs、ep-core/src/i18n.rs（NAMESPACES+include）、ep-daemon/src/api/packs.rs（stub router）、api/mod.rs、execution.rs 的 submit_direct stub、workspace Cargo.toml | 全部 Rust 侧注册点 + stub |
| **S2 FrontSkeleton** | frontend/src/App.tsx、sidebar.tsx、mobile-nav.tsx、api/client.ts、api/types.ts、i18n/index.ts、desktop app.rs 的 Page/NAV/AppCmd/AppMsg 新变体 | 全部前端/桌面注册点 + 类型 |

### Wave 1 — 地基（6 Agent 并行，峰值 6）

| Agent | 独占文件 | 职责（对应缺口） |
|---|---|---|
| **A1 DepStack** | ep-core/src/env.rs、config.rs | UV_CACHE_DIR/constraints/link-mode 注入 + 哈希扩展 + §8.3 全部配置字段 + PUT 合并语义的 config 层支持（§3.1；P2-18） |
| **A2 EnvInject** | ep-core/src/process.rs、deps.rs、ep-daemon/src/main.rs（run-module 段）、ep-desktop/src/main.rs（StartModule/Download env 段） | CUDA 库注入（**平台分支：Linux LD_LIBRARY_PATH / Windows PATH 前置 runtime/cuda-libs**，§15）+ compute.env 接线 + check_torch_cuda 补 env + run-module EP_ 前缀/占位符修复（P0-3）+ 桌面端 env 修复（P0-4 前置） |
| **A3 PackSchema** | crates/ep-pack/src/{lib,manifest}.rs、ep-core/src/model_id.rs | 包清单类型 + 校验 + QualifiedId 解析/归一 + serde 往返测试（§4.2/4.3） |
| **A4 PackIO** | crates/ep-pack/src/{build,extract,checksum}.rs | 打包/解包/checksum + 路径安全（参照 upload.rs 模式独立实现）+ zip-slip 攻击测试 |
| **A5 Detectors** | ep-core/src/compute/（rocm.rs、openvino.rs、directml.rs 新建，mod.rs、cpu.rs、cuda.rs Windows 分支修改） | ROCm/OpenVINO/DirectML 检测器（含 Windows 侧 intel-npu-smi/xpu-smi 探测）+ CPU refresh + fixture 测试（P1-13/P2-14；§7；Windows PC 真机验证清单随交付） |
| **A6 ModelMeta** | ep-core/src/model.rs、module/manifest.rs | meta tags/qualified_id/pack_id 字段 + 变体级 vram 字段 + active_models 解析 + cleanup_hf_cache 待接线准备 + size_bytes TODO（P2-10） |

### Wave 2 — 服务层（7 Agent 并行）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **B1 PackImport** | crates/ep-pack/src/import.rs | 导入编排（§4.4 全流程）+ 适配报告生成 + 集成测试 |
| **B2 DaemonPacks** | ep-daemon/src/api/packs.rs、state.rs | packs 路由实现 + 注册表 + WS pack_import + ProcessManager 代理注入接线（P1-8，state.rs 单点） |
| **B3 ExecEngine** | ep-daemon/src/execution.rs、ep-daemon/src/api/pipelines.rs、ep-core/src/task_registry.rs（新建）、ep-core/src/pipeline/vram.rs（新建） | max_parallel 全局闸门 + 管线级 max_instances semaphore + queued 状态（P1-3/§6.8）+ 任务/节点超时 + 取消路径 + **任务注册表 + 产物归集下沉 ep-core**（daemon/桌面共用，P1-4，含 pipeline_id 索引与落盘持久化）+ progress 带 task_id（P2-7）+ pipelines/{id}/tasks + vram-budget 端点（P1-5 替代）+ submit_direct 实现 + wait 同步模式 + callback_url 回调（§6.5） |
| **B4 DirectExec** | ep-daemon/src/api/execute.rs、api/upload.rs（input 上传段）、ep-daemon/src/api/autostart.rs（新建） | /api/execute/single + 退化 DAG 编译 + /api/upload/input + **模块自动拉起公共件**（execute 与 single 两路共用，§6.5） |
| **B5 ModulesApi** | ep-daemon/src/api/modules.rs | ModuleResponse +capabilities/device（P0-1/P2-3/P2-4）+ active_models 变体端点 + cache_dir 硬编码修复（P2-9） |
| **B6 ModelsApi** | ep-daemon/src/api/models.rs | tags 端点 + 下载取消（P2-6）+ max_concurrent semaphore（P2-1）+ pack 来源展示 |
| **B7 LLMNode** | ep-core/src/pipeline/executor.rs、dag.rs、ep-daemon/src/pipeline_bridge.rs | OpenAI 兼容 LLM builtin 节点（§6.7：chat/completions 客户端、api_key_env、prompt 占位符、错误语义）+ external_api 残留清理（旧名 alias）+ ffmpeg args 字符串兼容（P0-2 后端侧，防御性）+ 单测/集成测试 |

### Wave 3 — UI 层（8 Agent 并行，峰值 8 ← 全程最高）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **C1 UnifiedPage** | frontend/src/pages/models.tsx、hooks/use-models.ts、hooks/use-direct-exec.ts（新建） | 统一页（§5.1）+ tag + 变体选择器 + 直跑抽屉；旧 modules.tsx/module-detail.tsx 改造为卡片/抽屉视图 |
| **C2 PipelineNode** | frontend/src/components/shared/pipeline-node.tsx、pipeline-sidebar.tsx | capability 数据驱动 + ffmpeg args 数组化 + output_extension + manifest 参数渲染（P0-1/P0-2） |
| **C3 PipelinePage** | frontend/src/pages/pipeline.tsx | VRAM 每设备账本（§6.3）+ device 绑定 UI + 变体 pin + 导出/导入 + WS task_id 过滤 + 示例模板修复（P1-16） |
| **C4 DesktopCore** | ep-desktop/src/main.rs、app.rs | StartModule env 接线（P0-4）+ 下载前 ensure_venv（P0-5）+ LogLine 生产（P1-7）+ 任务拉取/直跑 AppCmd（P1-6）+ 调度器接线（P1-1 桌面侧）+ **管线执行接入**（background_loop 直连 ep-core runner + task_registry，产物落 workspace/tasks） |
| **C5 DesktopUI** | ep-desktop/src/pages/*.rs | 统一页镜像（卡片+tag+变体+直跑抽屉）+ **管线编辑器功能补齐（决策点 2）**：节点 palette（builtin+模块数据驱动）、连线交互与类型校验、参数编辑（manifest schema 驱动）、TOML 保存（ep-core 序列化已具备）、执行按钮与节点状态回显、VRAM 账本（ep-core vram helper）+ 任务页产物列表/打开 + 设备列真实数据 |
| **C6 PackCLI** | crates/ep-pack-cli/（新建） | new/validate/build/import/info/export 六命令 + 集成测试（复用 ep-pack crate） |
| **C7 SettingsTheme** | frontend/src/pages/settings.tsx、hooks/use-config.ts、store/theme.ts、components/layout/header.tsx、ep-daemon/src/api/config.rs | PUT 合并语义落地（P1-9）+ theme 三端同步（P2-2）+ 死配置处置（接线 log_level/check_updates，隐藏无消费者的，P2-1）+ requires_restart 标记 |
| **C8 DocsI18n** | i18n/locales/**（唯一写入口，规则 8）、docs/*.md | 汇总各 Agent 键需求清单落盘 zh/en 双份 + 键集门禁通过；文档先行产出：节点开发指南（PIPELINE_SPEC）、整合包作者指南、自动化集成指南（watcher）、CONFIG_REFERENCE 新配置章、MODULE_SPEC 更新 |

### Wave 4 — 硬化（4 Agent 并行）

| Agent | 独占文件 | 职责 |
|---|---|---|
| **D1 E2E** | crates/ep-pack/tests/、daemon 集成测试、测试 fixture | 测试包构建（小模型 bundle + reference 混合）→ 导入往返 → 直跑 E2E → video-to-srt 回归 → VRAM 计算 fixture 验证 |
| **D2 DeadCode** | 审计报告 §4 清单所列文件（state.runner 死接线、placeholder.tsx、status-badge.tsx、client.ts 死方法、过时注释等） | 死代码清除 + 注释对齐（不改行为，每项附证据行号）。**例外：ModuleProcess/ModuleLifecycle/ComputeScheduler 三件保留**（决策点 3，后续另行讨论精简） |
| **D3 Packaging** | packaging/、scripts/、build.sh、build.ps1 | entrypoint.service ExecStart 对齐（P2-17）+ entrypoint.install 文案 + start.sh/service 注入 runtime/cuda-libs + 打包纳入 cuda-libs/ + **watcher 示例脚本 scripts/examples/（决策点 6）** + build.ps1 与 build.sh 门禁等价性核对 |
| **D4 StackContent** | config/constraints.txt（新建）、modules/deep-filter/requirements.txt | constraints 内容定稿（torch 全家桶锁版本，Windows/Linux 同文）+ deep-filter torchaudio 兼容处置（pin 或文档化 scipy 保底）+ 迁移说明（旧 venv/cache 清理步骤） |

### Wave 5 — 门禁与交付（编排者）

全量 `./build.sh server` + `./build.sh gui`（clippy + workspace test + 前端 build）→ 跨波合并冲突清理 → D1 E2E 全绿复跑 → PROGRESS.md/README 更新 → 提交（提交粒度按波次，模型/venv/测试产物不入 git）。

---

## 11. 并行度与时间线

```
Wave S   ████                       S1 ∥ S2                          并行度 2
Wave 1   ████████████               A1∥A2∥A3∥A4∥A5∥A6                并行度 6
Wave 2   ██████████████             B1∥B2∥B3∥B4∥B5∥B6∥B7             并行度 7
Wave 3   ████████████████           C1∥C2∥C3∥C4∥C5∥C6∥C7∥C8          并行度 8 ← 峰值
Wave 4   ████████                   D1∥D2∥D3∥D4                      并行度 4
Wave 5   ██████                     编排者（门禁 + E2E + 交付）

Agent 总数: 27（含骨架 2）    峰值并行: 8    波次: 6
```

关键路径：S1 → A3/A4 → B1/B2 → D1（整合包主链）；A1/A2 与 UI 波次无阻塞关系。同波文件所有权零重叠：ep-core 内 env.rs/config.rs→A1、process.rs→A2、compute/→A5、model.rs+manifest.rs→A6（Wave 1）；executor.rs/dag.rs/pipeline_bridge.rs→B7、task_registry.rs/vram.rs/execution.rs→B3（Wave 2）；ep-pack 新 crate 全程无冲突面。

---

## 12. 风险与缓解

| 风险 | 概率 | 缓解 |
|---|---|---|
| execution.rs 大改（B3）与既有任务测试冲突 | 中 | B3 独占该文件 + 契约冻结 submit 签名；门禁预留返工窗口 |
| 统一页重构（C1，models.tsx 1494 行）回归 | 中 | C1 保留 use-models hook 数据层，仅重构呈现；E2E 覆盖下载/上传/导入三路径 |
| 硬链接去重在某些文件系统失效 | 低 | uv 自动回退 copy；D1 验证 du 收益，未达预期则文档记录 |
| constraints 锁版本与未来模块依赖冲突 | 中 | constraints 文件用户可编辑/可停用（配置指向置空）；哈希联动保证变更生效 |
| ROCm 无真机验证 | 确定 | best-effort + fixture，文档明示；检测失败优雅降级为空设备。OpenVINO/DirectML 在用户 Windows PC（NPU/iGPU）真机验证 |
| Windows/Linux 双平台行为漂移 | 中 | 每波门禁两平台各跑一遍门禁命令（§15）；平台分支代码（venv 路径/库注入/检测器）必须带 cfg 测试 |
| i18n 键需求跨波累积遗漏 | 中 | 规则 8 强制键清单随交付物；键集门禁测试兜底 |
| 变体切换 × 执行中模块的竞态 | 中 | MVP 语义=报错+引导切换，不做执行中热切换（§5.2） |
| 整合包被恶意构造 | 中 | checksum + 路径清洗 + 不带代码 + 大小上限（§4.4） |

---

## 13. 交付物

| 交付物 | 说明 |
|---|---|
| ep-pack crate + ep-pack-cli | 整合包 SDK（库 + 命令行） |
| daemon packs/execute/tags API + WS 扩展 | §8 契约全部落地 |
| 统一页 + 直跑 + tag + 变体管理 | WebUI 与桌面端 |
| 管线可用化 + VRAM 账本 + 设备绑定 + 导入导出 + LLM 节点 | §6 全部（含 §6.7 OpenAI 兼容 LLM builtin） |
| 桌面端管线编排补齐 | palette/连线/参数/保存/执行/任务产物（决策点 2） |
| 依赖栈统一 | UV_CACHE_DIR + constraints + 硬链接 + cuda-libs 注入（Linux LD_LIBRARY_PATH / Windows PATH 双分支） |
| ROCm/OpenVINO/DirectML 检测器 + 调度器接线 | §7 本次落地项；CUDA/OpenVINO/DirectML 于 Windows PC 真机验证 |
| 审计 P0/P1 全修 + P2 清单处置 | reports/feature_audit_report.md 逐项销号 |
| 文档 | 节点开发指南（PIPELINE_SPEC）、整合包作者指南、自动化集成指南（watcher）、CONFIG_REFERENCE 新配置、MODULE_SPEC 更新、README、PROGRESS.md |
| watcher 示例脚本 | scripts/examples/（决策点 6），Linux bash + Windows PowerShell 双版 |
| 测试 | 每 Agent 内建测试 + D1 全链 E2E；build.sh/build.ps1 双平台全绿 |
| Git | 按波次提交，不含模型/venv/缓存 |

---

## 14. 决策记录（用户 2026-08-04 确认）

| # | 决策点 | 结论 |
|---|---|---|
| 1 | 统一页路由 | **保留 `/models`**，`/modules` 重定向（可接受） |
| 2 | 桌面端管线编排 | **完整功能补齐**：palette/连线/参数编辑/TOML 保存/执行/任务产物（C4+C5，§10） |
| 3 | `ModuleProcess`/`ModuleLifecycle`/`ComputeScheduler` | **全部保留**，精简后续另行讨论；调度器接线属功能修复（P1-1）照常进行 |
| 4 | `external_api` 节点 | **保留**，功能限定为接入 OpenAI 兼容 LLM 端点以简化代码与 UI（§6.7，B7） |
| 5 | builtin 注册表重构 | **不做，全面文档化**：节点开发指南覆盖 builtin 与 module 两条路径（C8） |
| 6 | watcher 示例 | **提供**：scripts/examples/ 双平台脚本 + 自动化集成指南（D3+C8） |

---

## 15. 运行环境与跨平台执行说明

### 15.1 环境事实

- **目标运行环境：Windows + Linux（不变）**，跨平台是硬约束（GUI 代码、venv 路径、库注入、检测器全部平台分支）
- **开发执行机：用户的 Windows PC**，具备 NVIDIA GPU + Intel NPU + Intel iGPU → CUDA/OpenVINO/DirectML 异构真机测试在此进行；ROCm 无 AMD 硬件，仅 fixture
- 本 Linux 服务器为规划环境与 Linux 侧验证环境（video-to-srt E2E 已于 2026-08-04 跑通）

### 15.2 波次门禁命令（执行机为 Windows）

```powershell
cargo clippy --workspace --all-targets   # 零警告
cargo test --workspace                   # 全绿（含 i18n 键集门禁）
cd crates/ep-webui/frontend; npm install; npm run build   # 前端构建 + oxlint
.\build.ps1 server; .\build.ps1 gui      # 打包门禁（Linux 侧等价：./build.sh）
```

### 15.3 平台分支清单（实现时逐项检查）

| 项 | Linux | Windows |
|---|---|---|
| CUDA 库注入（A2） | `LD_LIBRARY_PATH` 前置 `runtime/cuda-libs` | `PATH` 前置 `runtime/cuda-libs`（DLL 搜索序） |
| venv python（既有） | `bin/python` | `Scripts\python.exe` |
| 设备检测（A5） | nvidia-smi / rocm-smi / xpu-smi | nvidia-smi.exe / intel-npu-smi / xpu-smi / OpenVINO 查询 |
| spawn shell（既有） | `sh -c` | `cmd /C` |
| watcher 示例（D3） | bash + inotifywait | PowerShell + FileSystemWatcher |
| ffmpeg | 系统 ffmpeg | 系统 ffmpeg 或 modules/test-ffmpeg 自带 exe |

### 15.4 异构验证矩阵（Windows PC 执行）

| 验证项 | 设备 | 内容 |
|---|---|---|
| CUDA | NVIDIA GPU | faster-whisper GPU 推理、VRAM 账本、设备绑定 |
| OpenVINO | Intel NPU + iGPU | 检测器识别、设备列表呈现、绑定软约束回退 |
| DirectML | Intel iGPU | 检测器识别（与 OpenVINO 并存时的去重/优先级） |
| 多设备调度 | 全部 | least_memory 分配、`CUDA_VISIBLE_DEVICES` 类隔离（compute.env 接线） |

### 15.5 全新 checkout 准备（Windows PC）

Rust stable + Node 20+ + uv + ffmpeg（见 README 前置依赖表）；`runtime/`、`models/` 不入 git，首次运行自动重建 venv（依赖栈统一后：constraints 锁版本 + 应用内 uv 缓存硬链接去重）。Linux 侧既有模型/venv 资产不迁移，按需重新下载或走整合包导入。
