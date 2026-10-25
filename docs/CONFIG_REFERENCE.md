# 配置参考 (Configuration Reference)

> 版本：1.5 | 适用于 EntryPoint v0.x
>
> v1.5 变更（T-26）：新增 `[storage]` + `[[storage.stores]]`——存储池逻辑名
> （计划 7 S0）。文件入口（file_input.path / file_output.path /
> file_archive.dest_dir / watcher 的 watch_dir·dest_dir / 直跑 input_path）
> 可用 `store:<逻辑名>/<相对路径>` 引用，服务端解析为物理路径；配套目录浏览
> API（`GET /api/stores/{name}/browse`）。旧配置文件无此段时照常解析（行为零变化）。
> 另：T-27 新增 `runtime/webhooks.json`（Webhook 触发器注册表，见 §4）。
>
> v1.4 变更（T-24）：新增 `[llm]` + `[[llm.providers]]`——命名 LLM 提供商档案
> （OpenAI 兼容 base_url + api_key + 超时/限速/代理/开关），经 WebUI 设置页或
> `/api/llm/providers` CRUD 管理；旧配置文件无此段时照常解析（行为零变化）。
>
> v1.3 变更（PLAN_TRIGGER_UNIFIED_LOG §5.7）：新增 `[general].log_retention_days`
> （`runtime/logs/` 下事件日志与模块日志的保留天数，0 = 永久）；目录结构总览
> 补 `runtime/logs/`（模块日志 + 统一事件日志）、`runtime/tasks/`（任务注册表）、
> `runtime/watchers.json`（触发规则注册表），并修订「不产生 logs/ 目录」的过时注释。
>
> v1.2 变更（PACK_UNIFY_PLAN §3/§8.3）：新增 `[python].uv_cache_dir` /
> `[python].constraints`、`[compute].cuda_libs_dir`、`[packs].staging_dir`、
> `[active_models]`；`PUT /api/config` 深度合并语义与 `requires_restart` 标记；
> `compute.env` 后端环境变量注入已实现（§3.2）；下载并发闸已生效（§1.4）。

本文档是 EntryPoint 所有配置项、环境变量和内部文件格式的完整参考。

---

## 1. 全局配置 (`config/app.toml`)

### 1.1 `[general]` — 通用设置

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `language` | string | `"zh-CN"` | 界面语言（`zh-CN` / `en-US`） |
| `theme` | string | `"dark"` | 主题（`dark` / `light`） |
| `log_level` | string | `"info"` | 日志级别（`trace` / `debug` / `info` / `warn` / `error`） |
| `check_updates` | bool | `true` | 启动时检查模块更新 |
| `log_retention_days` | u64 | `90` | 统一日志保留天数：`runtime/logs/` 下**统一事件日志**（`events-YYYY-MM.jsonl`）与**模块日志**（`*.log`）按文件 mtime 清理巡检（daemon 每小时一轮）的保留窗口。**`0` = 永久保留**（跳过清理）。daemon 自身运行日志不落盘（控制台 / journal），不受此项影响 |

```toml
[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true
log_retention_days = 90   # 0 = 永久保留
```

### 1.2 `[compute]` — 计算设备

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `strategy` | string | `"least_memory"` | 设备分配策略 |
| `disabled_backends` | string[] | `[]` | 禁用的计算后端列表 |
| `refresh_interval_secs` | u32 | `2` | 设备状态刷新间隔 |
| `allow_overcommit` | bool | `true` | 允许显存超额分配（仅警告） |
| `cuda_libs_dir` | string | `"runtime/cuda-libs"` | 共享 CUDA 库目录（§3.1 依赖栈统一）。启动模块时前置注入（Linux `LD_LIBRARY_PATH` / Windows `PATH`），多模块共用同一份 cuBLAS 等库；空字符串 = 不注入；相对路径基于应用根目录 |

**strategy 可选值：**

| 值 | 说明 |
|---|---|
| `manual` | 用户在 UI 中为每个模块手动指定设备 |
| `least_memory` | 自动选择剩余显存最大的设备 |
| `round_robin` | 轮询分配到各设备 |
| `single` | 所有模块使用同一设备（需配合 `single_device`） |

```toml
[compute]
strategy = "least_memory"
disabled_backends = []
refresh_interval_secs = 2
allow_overcommit = true
cuda_libs_dir = "runtime/cuda-libs"
# single_device = "cuda:0"    # strategy = "single" 时指定
```

### 1.3 `[ports]` — 端口管理

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `range_start` | u16 | `18000` | 端口范围起始 |
| `range_end` | u16 | `19000` | 端口范围结束 |

```toml
[ports]
range_start = 18000
range_end = 19000
```

### 1.4 `[models]` — 模型管理

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `cache_dir` | string | `"models"` | 模型缓存目录（相对或绝对路径） |
| `cache_paths` | string[] | `[]` | 本地模型缓存搜索路径（按优先级排序），用于发现用户已有的模型文件 |
| `hf_endpoint` | string | `""` | HuggingFace 镜像站 URL（空=官方）。**仅对 HuggingFace 源生效**（下载时注入 `HF_ENDPOINT`） |
| `default_source` | string | `"huggingface"` | 默认下载源（见下方生效规则） |
| `max_concurrent_downloads` | u32 | `2` | 最大并行下载数。已生效：下载并发闸（Semaphore），超额请求以 `queued` 状态排队，空位释放后按提交顺序自动启动；运行时改小不影响在途下载 |

**`default_source` 生效规则：**

- 可选值：`huggingface` / `modelscope` / `url`
- 下载请求**未显式指定下载源**时，若该值落在模型的可用来源（主源 + `[[models.mirrors]]` 镜像）之内，则使用它；否则回退模型声明的主源
- 下载请求显式指定了下载源（如 WebUI 中手动选择）时，以请求为准

**备选下载源（`[[models.mirrors]]`）：**

每个模型可在 module.toml 中声明若干备选源，主源不可用时（如网络受限）可切换。格式与 `[[models]]` 完整声明见 [MODULE_SPEC.md](MODULE_SPEC.md)：

```toml
[[models]]
id = "large-v3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-large-v3"
# revision = "master"   # 可选，缺省用该来源的默认分支
```

内置模块现状：faster-whisper 的 large-v3 / medium / small 均已配置 ModelScope 镜像（`pengzhendong/*`）；deep-filter 的 df3 模型 URL 源已指向 HuggingFace 仓库资产（`Serkan007/DeepFilterNet3-ONNX`，原 GitHub release 资产失效）。

> **已知限制（首次下载与 venv 准备）**：模型下载前会自动准备模块的 Python 虚拟环境。全新安装时该步骤耗时取决于依赖规模（含 torch 的模块约 15–20 分钟），可能超过常见 HTTP 客户端超时。建议将客户端超时设为 ≥20 分钟，或超时后直接重试——venv 已存在时下载会立即开始。

```toml
[models]
cache_dir = "models"
# cache_dir = "D:/AI_Models"
cache_paths = []
hf_endpoint = ""
# hf_endpoint = "https://hf-mirror.com"
default_source = "huggingface"
max_concurrent_downloads = 2
```

### 1.5 `[python]` — Python 环境

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `path` | string | `""` | Python 解释器路径（空=自动检测） |
| `uv_path` | string | `""` | uv 可执行文件路径（空=自动检测） |
| `uv_cache_dir` | string | `"runtime/.uv-cache"` | uv 缓存目录（依赖栈统一，PACK_UNIFY_PLAN §3.1）。对模块 venv 安装注入 `UV_CACHE_DIR`：缓存与 venv 同盘时硬链接生效，跨模块同版本依赖只占一份物理空间；缓存随应用目录可移植。**属重启敏感项**（影响后续 venv 构建） |
| `constraints` | string | `"config/constraints.txt"` | 全局 pip constraints 文件（锁 torch 全家桶等版本，保证多模块解析到同一版本 → 硬链接去重）。`uv pip install` 追加 `-c <file>`；文件不存在则跳过；**显式空字符串 = 停用**。constraints 内容变化会触发依赖哈希变更 → 自动重装 |

```toml
[python]
path = ""
uv_path = ""
uv_cache_dir = "runtime/.uv-cache"
constraints = "config/constraints.txt"
# path = "C:/Python312/python.exe"
# uv_path = "C:/Users/me/.local/bin/uv.exe"
# constraints = ""   # 显式停用 constraints
```

**自动检测顺序：**
1. 用户指定路径（非空时）
2. 系统 PATH 中的 `python3` / `python`
3. uv 管理的 Python（`uv python find`）

### 1.6 `[pipeline]` — 管线引擎

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `max_parallel` | u32 | `4` | 最大并行节点数 |
| `default_timeout_secs` | u32 | `600` | 任务级**空闲看门狗**超时（秒）：任务持续此时长无任何节点进度/心跳才判死（`0` = 停用看门狗）。**不再是任务总时长硬上限**——只要执行器持续产生心跳（节点开始/完成/失败，及长调用期间的周期心跳），任务可运行任意时长 |
| `default_node_timeout_secs` | u32 | `0` | 节点级**硬超时**全局缺省（秒）：节点未声明 `timeout_secs` 且管线未声明 `[pipeline] node_timeout_secs` 时，作为单节点 wall-clock 硬超时。`0`（缺省）= 跟随 `default_timeout_secs`（旧配置行为不变） |
| `keep_workspace` | bool | `true` | 任务完成后保留工作目录 |
| `workspace_dir` | string | `"workspace"` | 工作目录路径 |

```toml
[pipeline]
max_parallel = 4
default_timeout_secs = 600
default_node_timeout_secs = 0
keep_workspace = true
workspace_dir = "workspace"
```

### 1.7 `[server]` — HTTP 服务（daemon）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `host` | string | 代码缺省 `"0.0.0.0"`；仓库自带配置模板为 `"127.0.0.1"`（仅本机） | 监听地址 |
| `port` | u16 | `9800` | 监听端口（WebUI 与 REST API） |
| `allow_public` | bool | `false` | 是否允许公网访问。`false` 时启用 IP 过滤，仅放行 RFC 1918 私有地址 |
| `access_protection` | `"none"` \| `"token"` | `"none"` | 访问保护模式。`token` 时 `/api/*` 与 `/ws*` 需提供共享令牌；`/api/v1` 不重复校验——由 `[api].token` 守护，未配置时回退使用本令牌（两者皆空则返回 `401`）。页面/静态资源不受影响 |
| `token` | string | `""` | `token` 模式使用的共享令牌。建议随机长字符串；为空时拒绝所有受保护请求（失败关闭） |

```toml
[server]
host = "0.0.0.0"
port = 9800
allow_public = false
access_protection = "none"
token = ""
```

令牌可经 `Authorization: Bearer <token>` 或 `X-API-Key: <token>` 提供；WebSocket 握手与 GET/HEAD 原生加载（图片/视频/下载链接）额外支持 `?token=`，POST/PUT/DELETE 不接受 query token。**令牌进入 URL 可能被访问日志、浏览器历史或 Referer 泄漏，仅建议在内网或反向代理 + TLS 下使用。** 公网暴露前建议配置随机长令牌。

**`/api/v1` 鉴权（重要）：** `access_protection` 不重复校验 `/api/v1`——该前缀优先由 `[api].token` 单独守护（见 `[api]` 一节，全局令牌不被 v1 接受）。令牌模式下若 `[api].token` 未配置，v1 回退使用 `[server].token` 校验（含 `[api].enabled = false` 的情形），匿名盲区已关闭；此时若两个令牌都为空，所有 v1 请求失败关闭（返回 `401`）。非令牌模式且未配置 `[api].token` 时，v1 维持直通。需要 v1 独立凭据时，显式配置 `[api].token` 即可恢复独立鉴权语义。

### 1.8 `[network]` — 网络代理

统一控制模型下载、依赖安装与模块子进程的出口代理。取代此前"子进程隐式继承 daemon 环境变量"的不可控方式：所有需要联网的子进程显式注入这里配置的环境变量。

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `http_proxy` | string | `""` | HTTP 代理地址。空 = 不覆盖（继承 daemon 进程的系统环境变量） |
| `https_proxy` | string | `""` | HTTPS 代理地址。空 = 不覆盖（同上） |
| `no_proxy` | string | `"localhost,127.0.0.1"` | 不走代理的地址列表 |

**注入规则：**

- 非空字段同时注入大写 + 小写两套键（`HTTP_PROXY`/`http_proxy`、`HTTPS_PROXY`/`https_proxy`、`NO_PROXY`/`no_proxy`），兼容不同工具的探测习惯
- 字段为空则不注入对应键，不覆盖子进程从 daemon 继承的环境变量
- 注入目标：模型下载 Python 子进程、uv/pip 依赖安装进程、模块运行子进程

**生效时机：** 新启动的子进程。已运行的模块不受影响（需重启模块生效）。

```toml
[network]
http_proxy = ""
https_proxy = ""
no_proxy = "localhost,127.0.0.1"

# 有代理时示例：
# http_proxy = "http://127.0.0.1:7890"
# https_proxy = "http://127.0.0.1:7890"
```

### 1.9 `[packs]` — 整合包（PACK_UNIFY_PLAN §8.3）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `staging_dir` | string | `".pack-staging"` | 整合包导入/构建暂存根目录（相对路径基于应用根目录）。解包 → CHECKSUMS 校验 → 落位均在此目录内完成，结束后清理 |

```toml
[packs]
staging_dir = ".pack-staging"
```

### 1.10 `[active_models]` — 每模块激活变体（版本单槽位）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `<module_id>` | string | — | 该模块当前激活的模型变体 id（`[[models]].id`）。键值对形式，每模块一条 |

**单槽位语义**（PACK_UNIFY_PLAN §5.2）：每模块同一时间一个激活变体。
daemon 启动模块时按三级回退选择模型：`[active_models]` 配置 → manifest
`default = true` → 首个变体。变体切换端点
（`PUT /api/modules/{m}/models/{mid}/variant`，旧 `/api/models/{m}/{mid}/variant` 为弃用别名）写入本表并落盘；激活变体与管线节点
`model` pin 不一致时执行前报错并引导切换（不静默热切换）。

```toml
[active_models]
faster-whisper = "large-v3"
qwen3-tts = "default"
```

### 1.11 `[api]` — 统一推理 API（`/api/v1/*`）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | bool | `true` | v1 门面总开关。`false` 时跳过 v1 自身鉴权（直通）；但全局令牌模式下仍回退使用 `[server].token` 校验（见 `[server].access_protection`） |
| `token` | string | 无（不启用鉴权） | 可选 Bearer token。配置后 `/api/v1/*` 端点要求 `Authorization: Bearer <token>` 或 `X-API-Key: <token>`，不匹配返回 `401`；未配置时，全局令牌模式下回退使用 `[server].token` 校验，非令牌模式则维持直通 |
| `batches_allow_external_folders` | bool | `false` | 批量端点（`POST /api/v1/batches`）的 folder 准入范围。`false` = 仅限 workspace/uploads 内；`true` = 允许任意服务器可读目录（单机/可信局域网部署的便捷开关） |

仅保护 `/api/v1/*` 外部契约层；`/api` 其余 WebUI 内部端点不受影响。
端点用法、错误码与演进路线详见 `docs/INFERENCE_API.md`。

```toml
[api]
enabled = true
# token = ""   # 对外/公网暴露前建议配置随机长字符串
```

### 1.12 `[modules]` — 模块生命周期（空闲自动下线）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `idle_timeout_secs` | u64 | `1800` | 空闲自动释放阈值（秒）：运行中模块持续无任务触达超过该时长即自动停止，释放模型内存/显存与功耗；下次任务经既有拉起路径按需重载。`0` = 停用（7×24 常驻） |

**巡检机制：**

- daemon 每 **30 秒**巡检一轮运行中模块；空闲基准 = `max(进程启动时刻, 最近触达时刻)`
- **活跃触点**（以下五类事件刷新对应模块的"最近触达时刻"，重置空闲时钟）：
  1. 手动启动模块（模块页「启动」）
  2. 任务提交（管线/直跑入队即视为触达其引用的全部模块）
  3. 节点开始执行（on_node_start 回调）
  4. 节点执行完成（on_node_complete 回调）
  5. 任务进入终态（completed/failed/cancelled 的 finalize 阶段）
- **排队/运行任务引用豁免**：被任何排队或运行中任务引用的模块一律不回收，
  即使空闲时钟已超限
- **回收动作**：停止模块进程（杀进程树）并释放所分配端口；**venv 与模型文件保留**——
  下次拉起无需重新准备 Python 环境，也无需重新下载权重
- **生效时机**：实时读取——运行期经 WebUI 设置页或 `PUT /api/config` 修改即时生效
  （含改为 `0` 停用），无需重启 daemon

```toml
[modules]
idle_timeout_secs = 1800   # 空闲 30 分钟自动下线；0 = 常驻
```

### 1.13 `[tasks]` — 任务产物保留（T-12）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `retention.enabled` | bool | `false` | 后台清理总开关；`false` = 完全不清理 |
| `retention.max_age_days` | u32 | `0` | 保留天数：任务进入终态超过该天数即删除整个任务目录；`0` = 不限 |
| `retention.max_total_gb` | u32 | `0` | `workspace/tasks/` 总容量上限（GiB）：超限时按完成时间从最旧开始删除终态任务目录；`0` = 不限 |

**清理机制：**

- daemon 启动后延迟 3 分钟首跑，之后每 **6 小时**巡检一轮 `workspace/tasks/*`
- **仅清理终态任务**（completed / failed / cancelled，以 `runtime/tasks/<task-id>.json` 注册表记录判定）；
  排队 / 运行中任务绝不触碰，注册表缺失或损坏的目录保守跳过；daemon 重启时被改判为
  「重启中断」的遗留任务按 failed 终态处理（可被清理）
- 不可判定终态的目录（运行中 / 排队中、注册表缺失或损坏）仍计入总容量，但不参与清理；
  若因此无法降到上限，本轮输出一条 `warn` 日志（含不可删目录数量与字节数）
- 先按年龄（`max_age_days`）删除超龄任务；再按总容量（`max_total_gb`）从最旧开始淘汰，
  直至满足上限；两个维度互相独立，`0` = 该维度不限
- **删除整个任务目录**：`workspace/tasks/<task-id>/` 连同 `files/` 产物一并删除
- 每次删除向统一事件日志（`runtime/logs/events-YYYY-MM.jsonl`）追加一条
  `task_retention_cleanup` 事件（含 `task_id` / `pipeline_id` / `status` / `reason` / `size_bytes`）
- **生效时机**：实时读取——运行期经 WebUI 设置页或 `PUT /api/config` 修改**下一轮巡检生效**，无需重启

```toml
[tasks.retention]
enabled = true
max_age_days = 30   # 终态任务保留 30 天；0 = 不限
max_total_gb = 50   # workspace/tasks 总容量上限 50 GiB；0 = 不限
```

### 1.14 `[modules.auto_restart]` — 模块故障自愈（T-14）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | bool | `false` | 自愈总开关；`false` = 进程异常退出后不自动重启（旧行为） |
| `max_backoff_secs` | u32 | `30` | 单次退避上限（秒）：连续失败重试按 2s、4s、8s… 翻倍，封顶该值；小于 2 时按 2 秒处理 |
| `circuit_breaker_after` | u32 | `5` | 连续失败熔断阈值：达到后停止自动重启并写事件日志，需手动启动解除；`0` = 不熔断（持续退避重试）；小于 2 时按 2 处理（至少保留一次自动重启机会） |

**自愈机制：**

- 监督循环每 **1 秒**采样模块状态，只处理「曾 Running → Error（进程退出 /
  健康检查失败）」的运行中死亡；任务前按需拉起（§6.5）路径不变，不引入退避
- 首次死亡**立即**重启（复用既有拉起路径，含 venv / 模型 / 端口准备与健康检查）；
  重启未恢复（启动失败或 10 分钟内再次死亡）后按 **2s 起 ×2** 退避重试，
  封顶 `max_backoff_secs`
- 模块**稳定运行 10 分钟**后失败计数与退避清零
- **手动停止优先**：在模块页点「停止」（`POST /api/modules/:id/stop`）会取消该模块
  待执行与在途的自愈重启，并清零失败计数/退避/熔断状态；迟到的重启结果按尝试序号
  丢弃，不会再被拉起。拉起准备期间点「停止」同样生效（本次拉起被取消）
- **结果校验**：自愈重启返回成功后会再核对本轮采样到的真实状态——模块若已再次退出
  （未处于 Running），按失败计入退避/熔断，不会静默停在 Error；单次重启任务超过
  300 秒未返回则按失败处理并释放调度（防止拉起入口卡死导致该模块永久不再自愈）
- 连续失败达 `circuit_breaker_after` 次 → **熔断**：停止自动重启，模块保持退出/
  错误状态不再被拉起，需用户在模块页手动「启动」解除（观察到非自愈发起的启动即复位）
- **事件日志**：每次重启尝试 / 结果写 `module_auto_restart`
  （`{module_id, attempt, stage: starting|succeeded|failed, backoff_secs, error?}`），
  熔断写 `module_circuit_breaker`（`{module_id, failures, threshold}`），
  均落 `runtime/logs/events-YYYY-MM.jsonl`
- **生效时机**：实时读取——运行期经 WebUI 设置页或 `PUT /api/config` 修改即时生效，
  无需重启

```toml
[modules.auto_restart]
enabled = true
max_backoff_secs = 30       # 退避上限 30 秒（2s 起、每次 ×2）
circuit_breaker_after = 5   # 连续失败 5 次熔断；0 = 不熔断
```

### 1.15 `[cache]` — 直跑结果内容寻址缓存（T-16）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | bool | `true` | 直跑缓存总开关；`false` = 既不读缓存也不写缓存（旧行为） |
| `max_gb` | u32 | `8` | 缓存目录 `workspace/cache/` 容量上限（GB，按 1024³ 字节换算）；`0` = 不限（不做 LRU 淘汰，需自行关注磁盘占用） |

**缓存契约：**

- **范围**：仅**直跑**任务（`POST /api/pipelines/execute/single`、
  `/v1/inference/{module}/{capability}`，内部 `pipeline_id` 为 `direct/…`）。
  管线任务（`/api/pipelines/execute`、`/pipelines/{id}/runs`）不参与缓存
- **键**：`sha256(module_id + capability + 规范化参数 JSON + 输入指纹)`；规范化 =
  对象键递归字典序、数组保持原序。输入文件 **≤ 512MB** 时用**内容 sha256**；
  超过 512MB 退化为 `size + mtime` 弱键（`meta.json` 中 `weak: true` 标注——
  同尺寸且同修改时间的不同内容会被视为同一输入）
- **命中**：不占用执行闸门，直接生成 `completed` 任务，产物硬链接（失败降级为
  复制并告警）到 `workspace/tasks/<new-task-id>/files/<node-id>/`，任务详情
  `cached_from` 标出原任务 id；完成回调与事件日志（`task_terminal`）照常触发
- **未命中**：照常执行；成功终态后把产物写入 `workspace/cache/<key>/`
  （`meta.json` + `files/<node-id>/<name>`），并按 `max_gb` 做 LRU 淘汰
  （按 `last_used_ms` 最旧优先，0 = 不淘汰）
- **旁路**：请求体可选 `bypass_cache: bool`（默认 `false`）；multipart 形态用
  文本字段 `bypass_cache=true|false`。旁路请求既不读也不写缓存
- **只读共享**：缓存产物为只读共享（入缓存后设为只读权限位，硬链接命中
  时任务侧文件随之只读，属预期）；产物不支持就地修改，变更内容请写新文件。
  无 `meta.json` 的孤儿条目目录超过 1 小时宽限期后由 LRU 扫描整体回收
- **异常降级**：指纹计算 / 缓存读写失败只写告警日志，不影响任务提交与结果
- **生效时机**：每次直跑实时读取配置，`PUT /api/config` 修改后下一次请求即生效

```toml
[cache]
enabled = true
max_gb = 8          # 0 = 不限（不淘汰，注意磁盘占用）
```

### 1.16 `[llm]` — 命名 LLM 提供商档案（T-24）

管理若干命名的 LLM 提供商（OpenAI 兼容），供后续 LLM 消费方按逻辑名引用。
经 WebUI 设置页「LLM 提供商」管理区或 `/api/llm/providers` CRUD 端点维护
（端点契约见 `docs/API_DESIGN.md` §LLM 提供商），改动即时落盘，无需重启。

| 字段（`[[llm.providers]]`） | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `id` | string | —（必填） | 唯一逻辑名，URL 路径段；创建后不可改名，重复 id 被 API 拒绝（409）。不允许空白与 `/` `\` `#` `?`，≤128 字节 |
| `name` | string | `""` | 显示名（空时 UI 回退显示 id） |
| `base_url` | string | —（必填） | OpenAI 兼容根地址（如 `https://api.openai.com`）；必须以 `http://` / `https://` 开头且不含空白。测试连接请求 `GET {base_url}/v1/models` |
| `api_key` | string | `""` | API 密钥。**明文落盘**（与 `[server].token` 同级的安全权衡，安全审计已接受）；所有 API 响应一律脱敏——`api_key` 恒为空串，是否已配置由 `api_key_set` 布尔标记表达 |
| `timeout_secs` | u64 | `60` | 请求超时（秒）；合法范围 1–86400 |
| `rpm` | u32（可选） | 无 | 请求速率上限（RPM）；缺省不限制 |
| `concurrency` | u32（可选） | 无 | 并发上限；缺省不限制 |
| `proxy` | string（可选） | 无 | 出口代理（如 `http://127.0.0.1:7890`）；缺省不使用 |
| `enabled` | bool | `true` | 启用开关；`false` = 档案保留但不被下游调用使用 |

**凭据脱敏（P0）**：`GET /api/config`、`GET/POST/PUT /api/llm/providers` 的
响应中 `api_key` 恒为空串 + `api_key_set` 标记，并带 `Cache-Control: no-store`；
更新时 `api_key` 空串/缺省 = 保留原值（防脱敏回传误清），清除必须显式
`clear_api_key: true`。明文仅存在于 `config/app.toml` 与 daemon 内存。

```toml
[llm]

[[llm.providers]]
id = "openai"
name = "OpenAI 官方"
base_url = "https://api.openai.com"
api_key = "sk-..."
timeout_secs = 60
# rpm = 60
# concurrency = 4
# proxy = "http://127.0.0.1:7890"
enabled = true

[[llm.providers]]
id = "local-ollama"
base_url = "http://127.0.0.1:11434"
# 无密钥的本地服务：api_key 留空即可
```

> **旧配置兼容**：配置文件无 `[llm]` 段时照常解析（`providers` 视为空列表），
> 行为零变化。`rpm`/`concurrency`/`proxy` 为当前及规划中消费方的预留字段，
> 解析与落盘均支持。

---

### 1.17 `[storage]` — 存储池逻辑名（T-26，计划 7 S0）

给服务器上的物理目录起**逻辑名**，文件入口按 `store:<逻辑名>/<相对路径>` 引用，
服务端解析为物理路径——换机/重装时只需在新机的 `[storage.stores]` 里把同名
逻辑名重绑到新物理路径，管线/触发器/定时计划原样可用（迁移关键：**导出**
管线时池内物理路径自动改写为 `store:` 逻辑名，见 `docs/PIPELINE_SPEC.md` §导出）。

| 字段（`[[storage.stores]]`） | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `name` | string | —（必填） | 唯一逻辑名（`store:` 引用形式中的 `<name>`） |
| `type` | string | `"local"` | `local` / `smb` / `nfs`。**S0 全部按 local 语义处理**——不做挂载管理，要求物理路径已在 OS 层挂载可达（Windows UNC / Linux fstab 由部署方负责）；smb/nfs 仅作标记 |
| `path` | string | `""` | 物理路径（相对路径基于应用根目录解析，与 `models.cache_dir` 同口径） |
| `enabled` | bool | `true` | 启用开关；禁用的池在解析 `store:` 引用时按「缺依赖」拒绝（422） |

```toml
[storage]

[[storage.stores]]
name = "media"
type = "smb"          # 已挂载的 NAS 共享（S0 按 local 语义处理）
path = "/mnt/nas/media"

[[storage.stores]]
name = "local-nvme"
path = "D:/data"      # Windows 本地盘
enabled = true
```

**`store:` 引用解析规则**：

- `store:media/incoming/a.wav` → `<media.path>/incoming/a.wav`；
  `store:media`（无相对部分）→ 池根本身；
- 相对部分含 `..` 分段、绝对路径注入（POSIX `/`、Windows 盘符 / UNC / 反斜杠
  前缀）→ 拒绝（400）；`.` 分段与冗余分隔符归一化丢弃；
- 逻辑名未配置 / 已禁用 → 422（§6.4 缺依赖同款「补配置」提示语义）；
- **物理路径始终向后兼容**：所有文件入口照常接受物理路径，`store:` 纯增量。

**目录浏览 API**（WebUI 选择器的数据源，返回条目名/类型/大小）：

- `GET /api/stores` — 存储池列表（含解析后物理路径）；
- `GET /api/stores/{name}/browse?path=<相对路径>` — 单层浏览；`path` 先经
  `sanitize_relative_path` 清洗（拒绝 `..` / 绝对注入），再校验拼接结果仍在
  池物理路径内（纵深防御）；目录不存在 → 404；
- `POST /api/stores/resolve` — `store:` 引用解析（缺池/禁用 → 422）；
- `POST /api/stores/locate` — 物理路径批量反向改写为 `store:` 逻辑名
  （池外为 `null`；管线 TOML 导出前的改写数据源）。

> **旧配置兼容**：配置文件无 `[storage]` 段时照常解析（`stores` 为空列表），
> 行为零变化。

---

## 2. 完整 app.toml 示例

```toml
# EntryPoint 全局配置
# 路径说明：相对路径基于应用根目录解析

[server]
host = "0.0.0.0"
port = 9800
allow_public = false
access_protection = "none"
token = ""

[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true
log_retention_days = 90

[compute]
strategy = "least_memory"
disabled_backends = []
refresh_interval_secs = 2
allow_overcommit = true
cuda_libs_dir = "runtime/cuda-libs"

[ports]
range_start = 18000
range_end = 19000

[models]
cache_dir = "models"
cache_paths = []
hf_endpoint = "https://hf-mirror.com"
default_source = "huggingface"
max_concurrent_downloads = 2

[python]
path = ""
uv_path = ""
uv_cache_dir = "runtime/.uv-cache"
constraints = "config/constraints.txt"

[pipeline]
max_parallel = 4
default_timeout_secs = 600
default_node_timeout_secs = 0
keep_workspace = true

[network]
http_proxy = ""
https_proxy = ""
no_proxy = "localhost,127.0.0.1"

[packs]
staging_dir = ".pack-staging"

[modules]
idle_timeout_secs = 1800

[modules.auto_restart]
enabled = false
max_backoff_secs = 30
circuit_breaker_after = 5

[tasks.retention]
enabled = false
max_age_days = 0
max_total_gb = 0

[cache]
enabled = true
max_gb = 8

[llm]
# 命名 LLM 提供商档案（T-24）；无提供商时可整节省略

# [[llm.providers]]
# id = "openai"
# name = "OpenAI 官方"
# base_url = "https://api.openai.com"
# api_key = "sk-..."
# timeout_secs = 60

[api]
enabled = true
# token = ""   # 可选：配置后 /api/v1/* 要求 Bearer / X-API-Key

[active_models]
# faster-whisper = "large-v3"   # 每模块激活变体（变体切换端点自动写入）
```

> 通过 WebUI「设置」页或 `PUT /api/config` 修改配置会**直接落盘**到 `config/app.toml`，重启后不丢失。
>
> **`PUT /api/config` 为深度合并语义**（PACK_UNIFY_PLAN §8.2）：请求体中缺省的
> 字段**保留原值**，只有显式给出的字段被更新——补丁式修改单个配置项不再需要
> 回传整份配置。重启敏感项（监听地址/端口、Python/uv 解释器路径等影响已建
> 运行时状态的字段）变更时，响应携带 `requires_restart: true` 标记，提示客户端
> 需重启 daemon 才能完全生效。

---

## 3. 环境变量

### 3.1 系统注入到模块进程的环境变量

| 变量 | 说明 | 示例 |
|---|---|---|
| `EP_ROOT` | 应用根目录 | `G:\AI_Applications\EntryPoint` |
| `EP_MODULE_DIR` | 模块目录 | `...\modules\faster-whisper` |
| `EP_MODULE_ID` | 模块 ID（已注入） | `faster-whisper` |
| `EP_MODEL_DIR` | 当前（激活变体）模型目录 | `D:\AI_Models\faster-whisper-large-v3` |
| `EP_MODELS_ROOT` | 模型缓存根目录（含所有变体子目录，供 `params.model` 变体覆盖解析，见 ADAPTER_API.md §1.3） | `D:\AI_Models` |
| `EP_MODEL_ID` | 当前模型 ID（已注入；模块无激活模型时缺省） | `large-v3` |
| `EP_HOST` | adapter 绑定地址（固定回环 `127.0.0.1`，根治 Windows 防火墙弹窗；adapter 以 `os.getenv("EP_HOST", "127.0.0.1")` 读取，见 ADAPTER_API.md §1.2） | `127.0.0.1` |
| `EP_PORT` | 分配端口 | `18001` |
| `EP_DEVICE` | 设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `EP_DEVICE_INDEX` | 设备索引 | `0` |
| `EP_BACKEND` | 计算后端 | `cuda` / `rocm` / `openvino` / `cpu` |
| `EP_WORKSPACE` | 任务工作区根目录（任务级子目录经 predict 请求参数传递） | `...\workspace` |
| `EP_LOG_LEVEL` | 日志级别（已注入，固定 `info`） | `info` |
| `EP_ENTRYPOINT` | 启动命令模板辅助变量：module.toml `[runtime].entrypoint`（占位符 `{entrypoint}`） | `adapter.py` |
| `EP_BINARY` | 启动命令模板辅助变量：module.toml `[runtime].binaries` 首个二进制路径（占位符 `{binary}`） | `bin/native-tool` |
| `EP_VENV_PYTHON` | 启动命令模板辅助变量：平台自适应的模块 venv Python 解释器路径（占位符 `{venv_python}`；Windows `Scripts\python.exe` / Linux `bin/python`） | `...\runtime\venvs\faster-whisper\Scripts\python.exe` |

### 3.2 计算后端相关环境变量（`compute.env` 接线，已实现）

启动模块进程时，按当前设备的后端读取 module.toml 的 `[compute.env].<backend>`
表，替换 `{device_index}` 占位符后注入（多卡隔离立即生效）。格式与每后端
键表见 MODULE_SPEC.md §2.3。

| 后端 | 典型变量 | 值 |
|---|---|---|
| CUDA | `CUDA_VISIBLE_DEVICES` | 设备索引（`{device_index}` 替换后） |
| ROCm | `HIP_VISIBLE_DEVICES` | 设备索引 |
| OpenVINO | `OPENVINO_DEVICE` | 设备名（`GPU.0` / `NPU.0`） |
| CPU | —（模块自定义，如 `TORCH_DEVICE`） | — |

> 共享 CUDA 库目录（`[compute].cuda_libs_dir`）另以搜索路径前置方式注入
> （Linux `LD_LIBRARY_PATH` / Windows `PATH`），不属于本表。

### 3.3 用户可设置的环境变量（影响 EntryPoint 自身）

| 变量 | 说明 | 默认 |
|---|---|---|
| `EP_ROOT` | 应用根目录覆盖（配置、模块、日志等均基于它解析） | 可执行文件位置推断，兜底当前工作目录 |
| `EP_UMA_TOTAL_MB` | 显式声明统一内存架构（UMA）池容量（十进制 MB，T-22）：设置后多池账本产出 `kind:"uma"` 池、§6.8b 准入账本按单一共享池记账；用于平台名无 GB10/Grace 特征的统一内存设备。不设置时走自动识别（Windows 注册表平台特征），识别失败 = 无 UMA 池、行为与旧版一致 | 未设置（自动识别） |

**由系统注入（用户不直接设置）：**

| 变量 | 说明 |
|---|---|
| `HF_ENDPOINT` | 由 `[models].hf_endpoint`（非空时）注入模型下载进程，仅 HuggingFace 源生效 |
| `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` 等 | 由 `[network]` 节注入，见 §1.9 |

**计划中（当前代码未实现，设置无效）：** `EP_CONFIG_DIR`（配置目录固定为 `<root>/config`）、`EP_LOG_DIR`（daemon 日志走 systemd journal / 控制台）、`HF_TOKEN`、`MODELSCOPE_CACHE`。后两者虽无显式处理，但仍可经 daemon 进程环境隐式继承到子进程（如通过 systemd `Environment=` 设置）。

---

## 4. 内部文件格式

### 4.1 `.ep_meta.json` — 模型元数据

位于模型缓存目录内每个模型文件夹下。

```json
{
  "module_id": "faster-whisper",
  "model_id": "large-v3",
  "source": "huggingface",
  "repo_id": "Systran/faster-whisper-large-v3",
  "revision": "main",
  "downloaded_at": "2026-07-20T10:30:00Z",
  "total_size_bytes": 3094850000,
  "qualified_id": "ep.systran.faster-whisper",
  "tags": ["字幕"],
  "pack_id": "pigeonfish.subtitle-kit"
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `module_id` | string | 所属模块 ID |
| `model_id` | string | 模型 ID（对应 module.toml 中 [[models]].id） |
| `source` | string | 获取来源（`huggingface` / `modelscope` / `url` / `pack`——整合包导入落位为 `pack`） |
| `repo_id` | string | 仓库 ID |
| `revision` | string | 版本/分支 |
| `downloaded_at` | string | 下载完成时间（ISO 8601） |
| `total_size_bytes` | u64 | 总大小 |
| `qualified_id` | string? | 全限定模型 ID（§4.3 PACK_UNIFY_PLAN；旧数据可缺省） |
| `tags` | string[] | 用户/整合包标签（统一页 chips 筛选，随整合包流转） |
| `pack_id` | string? | 来源整合包 id（可空；非 pack 来源无此字段） |

**用户可安全删除此文件**。删除后系统视为手动放置的模型。

### 4.2 `.ep_deps_hash` — 依赖哈希标记

位于 `runtime/venvs/<module-id>/` 下。

```
sha256:a1b2c3d4e5f6...
```

单行文本。哈希输入 = `requirements.txt` 字节 + constraints 文件字节（若存在）
+ link-mode 版本号（依赖栈统一，PACK_UNIFY_PLAN §3.1）：任一输入变化（改依赖、
改 constraints、换链接模式）都会触发 venv 重装。

### 4.3 `ep.lock` — 依赖锁定文件

位于 `runtime/venvs/<module-id>/` 下。

```
# ep.lock — 由 uv pip freeze 生成
fastapi==0.115.0
uvicorn==0.30.0
faster-whisper==1.1.0
...
```

精确版本锁定，用于跨机器还原相同环境。

### 4.4 任务工作目录结构

```
workspace/<task-id>/
├── task.json                  ← 任务元信息
├── input/
│   └── source.mp4             ← 输入文件（或符号链接）
├── extract/
│   └── output.wav             ← FFmpeg 输出
├── denoise/
│   └── output.wav             ← 降噪输出
├── asr/
│   └── output.json            ← ASR 结果
├── translate/
│   └── output.json            ← 翻译结果
└── srt/
    └── output.srt             ← 最终字幕
```

**task.json：**
```json
{
  "task_id": "abc123",
  "pipeline_id": "video-to-srt",
  "pipeline_name": "视频转字幕",
  "started_at": "2026-07-20T10:30:00Z",
  "completed_at": "2026-07-20T10:35:22Z",
  "status": "completed",
  "input_file": "C:/Videos/test.mp4",
  "nodes": {
    "extract": {"status": "completed", "elapsed_secs": 2.1},
    "denoise": {"status": "completed", "elapsed_secs": 15.3},
    "asr": {"status": "completed", "elapsed_secs": 180.5},
    "translate": {"status": "completed", "elapsed_secs": 45.2},
    "srt": {"status": "completed", "elapsed_secs": 0.1}
  }
}
```

---

## 5. 目录结构总览

```
EntryPoint/                        ← 应用根目录
├── bin/                           ← 二进制（Windows server 包布局；源码树为 target/release/）
│   ├── ep-daemon[.exe]            ← 主程序（daemon：托管 WebUI + REST API/WebSocket）
│   └── ep-pack[.exe]              ← 整合包 CLI
├── config/
│   ├── app.toml                   ← 全局配置（本文档 §1）
│   ├── constraints.txt            ← 全局 pip constraints（§1.5）
│   └── pipelines/                 ← 管线定义
│       ├── video_to_srt.toml
│       └── audio_extract.toml
├── modules/                       ← 模块目录
│   └── <module-id>/
│       ├── module.toml
│       ├── adapter.py
│       └── requirements.txt
├── runtime/                       ← 运行时（自动生成）
│   ├── venvs/
│   │   └── <module-id>/
│   │       ├── .ep_deps_hash
│   │       ├── ep.lock
│   │       └── ... (venv 内容)
│   ├── .uv-cache/                 ← uv 缓存（[python].uv_cache_dir，硬链接去重源）
│   ├── cuda-libs/                 ← 共享 CUDA 库（[compute].cuda_libs_dir）
│   ├── packs/                     ← 已装整合包注册表（<pack-id>.json）
│   ├── tasks/                     ← 任务注册表（原子落盘；重启后非终态任务改判 failed）
│   ├── watchers.json              ← 触发器规则注册表（原子落盘；WebUI 触发器页 CRUD）
│   ├── schedules.json             ← cron 定时计划注册表（原子落盘；管线编辑器与触发器页共用）
│   ├── webhooks.json              ← Webhook 触发器注册表（T-27；hook_id → 触发目标，原子落盘）
│   └── logs/                      ← 模块日志（<module>*.log）+ 统一事件日志
│       ├── <module>*.log          ←   daemon 捕获的模块子进程输出（WebUI 日志抽屉展示）
│       └── events-YYYY-MM.jsonl   ←   统一事件日志（watcher_trigger / task_terminal /
│                                        webhook_trigger，单行 JSON 追加、按月滚动；
│                                        log_retention_days 清理）
├── .pack-staging/                 ← 整合包导入/构建暂存（[packs].staging_dir，随用随清）
├── workspace/                     ← 管线任务工作目录
│   └── <task-id>/
└── docs/                          ← 文档
    ├── MODULE_SPEC.md
    ├── ADAPTER_API.md
    ├── PIPELINE_SPEC.md
    ├── CONFIG_REFERENCE.md
    └── WEBUI_GUIDE.md

<model_cache_dir>/                 ← 模型缓存（用户可指定位置）
├── faster-whisper-large-v3/
│   ├── .ep_meta.json
│   ├── model.bin
│   └── config.json
└── ...
```

---

## 6. 配置优先级

配置来源从高到低：

1. 运行时修改（WebUI 设置页 / `PUT /api/config`）——立即生效并落盘到 `config/app.toml`
2. `config/app.toml`（启动时加载）
3. 内置默认值（文件中缺失的字段用默认值补齐）

应用根目录的解析顺序：环境变量 `EP_ROOT` → 可执行文件位置推断（需含 `config/` + `modules/`）→ 当前工作目录。

---

## 7. 首次启动行为

```
1. 检测 config/app.toml 是否存在
   - 不存在 → 从内置模板生成默认配置
2. 检测/自动安装系统依赖（ffmpeg 等，仅 Linux 包管理器路径）；
   Python/uv 不做启动期检测——缺失会在首次模块 venv 准备/模型下载时
   报错（模块页与 daemon 日志可见）
3. 扫描 modules/ 目录
   - 解析所有 module.toml
   - 标记各模块状态（就绪/缺依赖/缺模型）
4. 检测计算设备
   - 枚举所有可用后端和设备
5. daemon 开始托管 WebUI 与 REST API（监听地址以 [server].host 为准，
   代码缺省 0.0.0.0；仓库自带配置缺省 127.0.0.1），本机浏览器访问 http://127.0.0.1:9800 即可使用；
   Windows server 包的 start-daemon.bat 会自动打开默认浏览器
```
