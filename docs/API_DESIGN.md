# API 端点设计原则与迁移表

> 本文是 `/api` 端点路径的设计基准：调研依据、命名原则、canonical 端点表、
> 以及旧路径的弃用/迁移策略。新增端点前请先对照 §2 的原则。
> 供应商中立的接入视角见 [API_INTEGRATION_GUIDE.md](API_INTEGRATION_GUIDE.md)。

## 1. 调研依据（2026-09）

对三类共 20+ 平台的公开 API 做了横向调研，作为本次优化的参照系：

- **云端媒体/推理平台**（Replicate、fal.ai、OpenAI、Hugging Face、Stability、
  ElevenLabs、AssemblyAI、Deepgram、Runway、Luma）：收敛出「`/v1` 根级版本
  + 复数集合 + POST 建任务 → GET `{id}` 轮询取产物 → POST `.../cancel` 取消
  + `/files` 换取输入引用 + 产物托管/相对 URL」的异步任务模式；
- **本地/自托管 AI 服务**（Ollama、vLLM、LM Studio、ComfyUI、A1111、InvokeAI）：
  现代化趋势是向编排阵营靠拢——InvokeAI `/api/v1/*`、ComfyUI 新增
  `/api/jobs/{id}/cancel`、LM Studio v0→v1；实时通道普遍为同端口 `/ws`，
  SPA 挂 `/`、API 收进 `/api`；
- **编排平台**（n8n、Airflow、Prefect、GitHub Actions）：「执行一次 = 建 run
  资源」收敛为 `POST /dags/{id}/dagRuns`（Airflow）、`POST .../dispatches`
  （GitHub）等形态；取消/重试一律是 run 上的动词子路径 POST；
- **设计规范**（Google AIP、Microsoft、Zalando、GitHub/Stripe 实践）：复数
  资源名词是强共识；非 CRUD 动作的主流表达是**动词子路径 + POST**（GitHub
  `/cancel`、Stripe `/finalize`；Google 的 `:verb` 冒号风格仅 gRPC 生态）；
  Zalando 建议路径段 kebab-case、子资源嵌套 ≤3 层、占位符带类型。

## 2. 设计原则（本仓库端点命名规则）

1. **版本化**：对外稳定契约挂 `/api/v1/*`（本地平台惯例，与 InvokeAI/n8n/
   LM Studio 一致）；WebUI 内部端点与 daemon 同仓同发，暂不版本化。注意
   `/v1` 仅是版本前缀，**不是** OpenAI 兼容契约（见 §5）。
2. **资源层级**：模型是模块的子资源——管理端点 canonical 为
   `/modules/{module_id}/models/...`；`GET /models` 仅作跨模块聚合目录
   （OpenAI `GET /v1/models` 式只读视图）。嵌套 ≤3 层。
3. **运行资源化**：「执行一次」建 run/task 资源，不在路径里放动词：
   `POST /pipelines/{pipeline_id}/runs`（Airflow dagRuns 同型）、
   `POST /modules/{module_id}/runs`；跨管线的任务查询走全局
   `/tasks` + 过滤参数。
4. **动作用动词子路径 + POST**：`start`/`stop`/`cancel`/`import`/`export`/
   `download`/`upload`/`cancel-download`/`check-update`，kebab-case。
5. **代理子资源**：模块自有扩展端点经 `/modules/{module_id}/proxy/{*path}`
   转发（K8s proxy 子资源惯例）。
6. **文件**：直跑输入上传 canonical 为 `POST /files`（Stripe/OpenAI files
   模式：multipart 换引用）。
7. **占位符带类型**：`{module_id}`、`{model_id}`、`{pipeline_id}`、
   `{task_id}`、`{pack_id}`、`{watcher_id}`，不裸用 `{id}`。
8. **兼容策略**：改名的旧路径保留为**弃用别名**（行为完全一致），前端与
   文档锁步迁移；别名移除至少保留一个大版本并在本文 §4 标注。

## 3. Canonical 端点表（v0.2 起）

### 系统

| 方法 | 路径 |
|---|---|
| GET | `/api/health` |
| GET | `/api/deps` |
| GET | `/api/devices` |
| GET | `/api/events`（支持 `?rule=&type=&limit=`） |
| GET / PUT | `/api/config` |
| GET | `/api/storage` |
| GET | `/api/metrics`（T-32，Prometheus 文本格式） |

`GET /api/metrics`（T-32）输出 Prometheus exposition format 0.0.4 文本
（`Content-Type: text/plain; version=0.0.4; charset=utf-8`，零第三方依赖手写
渲染）。与全部 `/api/*` 端点同受访问保护（token 模式下需凭据；Prometheus
抓取端可走 `?token=` 或 header）。指标以 `entrypoint_` 为前缀：

| 指标 | 类型 | 标签 | 口径 |
|---|---|---|---|
| `entrypoint_pool_total_mb` | gauge | `pool`, `device` | 池容量（MB）。`pool` ∈ `vram`（每块有容量指标的 GPU 设备一池）/ `ram`（单一池，`device="cpu"`，恒产出）/ `uma`（T-22 统一内存平台识别成功时产出，GPU 预算折叠进单一池、不产出逐设备 vram 池） |
| `entrypoint_pool_committed_mb` | gauge | `pool`, `device` | 运行中模块实例的声明预算之和（模块粒度，与 `/api/pipelines/vram-budget` 同源取数：manifest `resolve_vram_estimate`/`resolve_ram_estimate` 变体级优先）。排队任务的已承诺额不计（调度账本未暴露）；CPU 落位不计 vram；无容量指标设备上的预算不产出（准入 fail-open 同边界） |
| `entrypoint_tasks_total` | counter | `status` | 任务注册表终态计数（`completed`/`failed`/`cancelled` 三桶恒产出）。进程内随任务终态单调；重启时遗留 queued/running 改判 failed、retention 清理同步删除记录（F5）时计数可回落——按 Prometheus counter reset 语义聚合 |
| `entrypoint_queue_running` / `entrypoint_queue_queued` | gauge | — | 调度器权威在途量（与 `GET /api/queue` 同源） |
| `entrypoint_queue_paused` | gauge | — | 全局派发暂停标志（T-15），1 = 暂停 |
| `entrypoint_modules` | gauge | `status` | 可解析 manifest 的模块按生命周期三分类计数：`running`（Running/Starting）、`error`（上次启动失败）、`stopped`（其余，含从未启动）。三桶恒产出 |

不做时间序列，速率/窗口聚合交给 Prometheus（`rate()`/`increase()`）。自动
重启次数 counter 与 external 池（openai-bridge，M2 后）暂未产出：前者待
health_supervisor 暴露可读计数器，后者将以 `pool="external"` 标签接入。
external 池指标为代码注释占位（`api/metrics.rs` 模块头「边界」段）。


`GET /api/storage`（T-11）返回 `StorageReport`：`dirs[]` 为 `workspace` / `models` /
`tasks` 三个逻辑目录（`path` 为解析后的绝对路径，`used_bytes` 递归求和；子目录符号链接
不跟随、根目录 junction/symlink 跟随）。`workspace.used_bytes` 已包含其下的 `tasks`，
聚合总占用时应取 `workspace + models`。`free_bytes` 为模型缓存盘剩余字节，探测失败为
`null`，客户端应显示未知态而不是按 100% 处理。

`GET /api/config` / `PUT /api/config`（T-16）返回脱敏后的配置视图：

- `server.token` 与 `api.token` 恒输出空串；是否已配置由平级布尔字段
  `server_token_set` / `api_token_set` 表示，客户端不得从响应回读令牌明文。
- 响应头 `Cache-Control: no-store`（成功与 400 均包含）。
- PUT patch 中 `server.token` / `api.token` **为空串或省略 = 保持不变**；覆盖需传
  非空字符串；清除必须显式传 `clear_server_token: true` / `clear_api_token: true`
  （不使用 `"***"` 掩码哨兵，避免误覆盖）。
- PUT 响应额外含 `requires_restart`。`server.access_protection = "token"` 且合并后
  `server.token` 为空时返回 400（i18n 键 `apiCore.config.tokenRequired`），且内存
  配置回滚到请求前状态。

### 模块与模型

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/modules` | 模块清单 |
| POST | `/api/modules/{module_id}/start` \| `/stop` | 生命周期动作 |
| GET | `/api/modules/{module_id}/status` \| `/logs` | |
| GET | `/api/modules/{module_id}/models` | 模块内模型列表 |
| POST | `/api/modules/{module_id}/models/import` \| `/download` \| `/upload` | 模型获取 |
| POST | `/api/modules/{module_id}/models/{model_id}/check-update` \| `/cancel-download` | |
| PUT | `/api/modules/{module_id}/models/{model_id}/tags` \| `/variant` | 元数据与激活变体 |
| DELETE | `/api/modules/{module_id}/models/{model_id}` | |
| GET | `/api/models`、`/api/models/downloads` | 聚合目录 / 下载任务列表 |
| POST | `/api/modules/import`；GET `/api/modules/{module_id}/export` | 模块档案导入/导出 |
| GET\|POST\|DELETE | `/api/modules/{module_id}/proxy/{*path}` | 模块扩展端点代理 |

### 管线与运行

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/pipelines` | |
| GET / PUT / DELETE | `/api/pipelines/{pipeline_id}` | |
| GET | `/api/pipelines/{pipeline_id}/status` \| `/export` | |
| GET | `/api/pipelines/{pipeline_id}/runs` | 管线维度 run 列表 |
| POST | `/api/pipelines/runs` | **集合级 run 创建**（body 带 `pipeline_id` 或内联 `spec` 跑未落盘管线，二选一同传 400） |
| POST | `/api/pipelines/{pipeline_id}/runs` | **按 id 执行**（路径指名糖，路径优先于 body） |
| POST | `/api/pipelines/import`；PUT/GET/DELETE `/api/pipelines/{pipeline_id}/schedule` | |
| POST | `/api/pipelines/vram-budget` | 编辑器预算计算 |
| POST | `/api/modules/{module_id}/runs` | **单模型直跑**（run 资源化） |
| GET | `/api/tasks`、`/api/tasks/{task_id}` | 全局任务中心 |
| GET | `/api/tasks/{task_id}/artifacts[/{node_id}]` | 产物列举 / 下载 |
| POST | `/api/tasks/{task_id}/cancel` | |

产物下载默认带 `Content-Disposition: attachment`。`GET /api/tasks/{task_id}/artifacts/{node_id}?inline=1`
仅对白名单媒体扩展名（图片 png/jpg/jpeg/gif/webp/bmp/avif、视频 mp4/webm/m4v/mov/mkv、音频
wav/mp3/flac/ogg/m4a/aac/opus/wma）省略该头，供 `<img>`/`<video>`/`<audio>` 内联预览；
`svg`/`html` 永不内联。302 跳转到 `/api/task-files/...` 时透传 `inline=1` 与 `token`，其余 query 一律丢弃，
最终白名单仍由静态文件中间件校验。

模型下载预检（T-11）：`POST /api/modules/{module_id}/models/download` 在入队与实际
spawn 前检查模型缓存盘空间，需求按 `size_estimate_mb × 1.05` 估算；不足时返回
`507 INSUFFICIENT_STORAGE` 与 i18n 键 `apiModels.storageInsufficient`（文案携带
`required_mb` / `available_mb`）。排队任务在取得并发空位、启动下载前会再查一次，不足则
把该排队任务置为 `failed`（此时不再有 HTTP 响应可返回）。清单未声明
`size_estimate_mb`（缺省/0）或探测失败时放行：空间预检是保护而非硬门禁。

**直跑结果缓存（T-16）**：`POST /api/pipelines/execute/single`、
`POST /api/modules/{module_id}/runs` 与 `/api/v1/inference/...` 默认启用内容寻址
缓存。键 = `sha256(module_id + capability + 规范化参数 JSON + 输入指纹)`：
输入 ≤ 512MB 用内容 sha256，超过退化为 `size + mtime` 弱键（元信息 `weak: true`）。
**命中**不占执行闸门，直接生成 `completed` 任务，产物硬链接（失败降级复制）进
`tasks/{new_id}/files/`，任务详情 `cached_from` 指回原任务 id，回调/事件照常触发；
**未命中**执行成功后写 `workspace/cache/`，按 `[cache] max_gb` 做 LRU 淘汰。
请求体可选 `bypass_cache: true`（multipart 为文本字段）单次旁路；`[cache] enabled=false`
全局关闭。管线执行（`/api/pipelines/execute`、`/pipelines/{id}/runs`）不参与缓存。

**force 超支提交（T-22）**：执行系端点（`/api/pipelines/execute`、`/pipelines/runs` 系、
`/api/execute/single`、`/api/modules/{module_id}/runs`、`/v1/inference/{module}/{capability}`
JSON 与 multipart）请求体增可选 `force: true`（缺省 false；multipart 为文本字段
`"true"`）：跳过 §6.8b 动态 VRAM 准入的排队等待（模块并发上限与显存余额不参与判定，
队列中插队先于普通任务出队）；静态闸门（`max_parallel` / `max_instances`）与全局暂停
不受影响，安全上限之外不绕过任何清理/安全逻辑。force 任务的提交响应附加
`forced_overcommit: true` + `accepted_risk: "user"`，并按需附 `overcommit_pools`
（复用 vram-budget `pools` 形状的超支池明细，仅存在超支时出现）；`GET /api/tasks` 与
任务详情同步透出这两字段。**不传 force 时请求与响应行为逐字节不变**。
`POST /v1/batches` 不开放 force（批量本为无人值守整目录吞吐设计）。

**dry-run 预演与提交幂等键（T-54，计划6 6.1）**：Agent/自动化调用的两个
提交安全面，覆盖 `POST /api/execute/single`（canonical
`POST /api/modules/{module_id}/runs`）与 `POST /v1/inference/{module}/{capability}`
（JSON 与 multipart 双形态）：

- **dry-run 预演**：请求体可选 `dry_run: true`（multipart 为文本字段 `"true"`）——
  校验全部通过（模块/capability/参数 schema/输入存在性）后**不入队**，返回
  `200 {"would_submit": true, "dry_run": true, "plan": {…}}`。`plan` 摘要含
  `module_id` / `capability` / 校验+默认值注入后的 `params` / 目标 `device`
  （显式 hint 或调度器选择，`device_source: auto|explicit`）/ 解析后的
  `model_variant` / 预估显存内存（`vram_estimate_mb` / `ram_estimate_mb`，
  复用 vram-budget 同一取数路径：变体级优先、模块级兜底）/ 输入描述
  （`input.kind: path|text|file`）。**零副作用**：不建任务、不占端口、
  不拉模块、文本输入不物化；校验失败照常 400/404/409。
- **提交幂等键**：请求体可选 `idempotency_key`（string，≤128 字符，超长
  400）——同键重复提交在任务在案期返回**首个 task_id**，响应带
  `idempotent_replay: true` 标记（202，与首次提交同形状）。侧表为进程内
  实现（与 FORCE 任务登记同款）：任务终态后条目保留、**不跨重启**；查重
  前置于输入物化/模块拉起/入队，重放零副作用。不同键互相独立（各自新任务）。

### 机器可读规范（T-54）

| 方法 | 路径 |
|---|---|
| GET | `/api/openapi.json`（OpenAPI 3.1 机读规范导出） |

手写 serde 结构生成（零新依赖），覆盖 v1 门面主要端点（capabilities /
inference 提交 / tasks 查询取消 / batches·groups / 旧 result 别名）：每端点
提供 summary / 请求体形状摘要（含 dry_run / idempotency_key）/ 错误码表
（`x-error-codes` 扩展字段）。精度定位**机器可读路由发现**而非完整 schema
校验；字段级契约以 INFERENCE_API.md 为准。位于 `/api` 前缀下，token 模式
随全局访问保护一起校验。

### 其他

| 方法 | 路径 |
|---|---|
| GET + POST | `/api/watchers`；GET / PUT / DELETE `/api/watchers/{watcher_id}` |
| GET | `/api/packs`；GET / DELETE `/api/packs/{pack_id}`；GET `/api/packs/{pack_id}/export` |
| POST | `/api/packs/import` \| `/build` \| `/upload` |
| POST | `/api/files`（直跑输入上传） |
| WS | `/ws`、`/ws/logs`、`/ws/progress`（根级，无版本，ComfyUI/InvokeAI 惯例） |

### LLM 提供商（T-24；设置页管理区，配置字段见 CONFIG_REFERENCE.md §1.16）

| 方法 | 路径 |
|---|---|
| GET | `/api/llm/providers`（脱敏视图列表） |
| POST | `/api/llm/providers`（创建；重名 `id` → 409，校验失败 → 400） |
| PUT / DELETE | `/api/llm/providers/{provider_id}`（更新 / 删除；未知 id → 404） |
| POST | `/api/llm/providers/{provider_id}/test`（测试连接） |

`id` 为唯一逻辑名（URL 路径段，创建后不可改名）。凭据脱敏与 `/api/config`
的 token 同模式（P0）：

- 所有响应（含 400/404/409）带 `Cache-Control: no-store`；`api_key` 恒输出
  空串，是否已配置由 `api_key_set` 布尔标记表示。
- PUT 语义：缺省字段保留原值；`api_key` 空串/缺省 = 保留（防脱敏回传误清），
  非空 = 覆盖；清除必须显式 `clear_api_key: true`（优先于 `api_key` 字段）。
  `rpm` / `concurrency` / `proxy` 支持显式 `null` = 清除。校验失败 → 400 且
  内存/磁盘零改动；落盘失败 → 500 且内存回滚。
- `POST .../test`：daemon 代调 `GET {base_url}/v1/models`（OpenAI 兼容惯例，
  已配置密钥时附 Bearer；超时取 profile `timeout_secs`，配置 `proxy` 时经其
  发出）。恒返回 200，业务两态在 `ok` 字段：成功
  `{"ok":true,"elapsed_ms":…,"model_count":…}`，失败
  `{"ok":false,"elapsed_ms":…,"error":"…"}`（非 OpenAI 形状的响应体不影响
  成功判定，仅 `model_count` 缺失）。

### 对外稳定契约 `/api/v1`（token 鉴权；字段级定义见 INFERENCE_API.md）

| 方法 | 路径 |
|---|---|
| GET | `/api/v1/capabilities` |
| POST | `/api/v1/inference/{module_id}/{capability}` |
| GET | `/api/v1/tasks` |
| GET | `/api/v1/tasks/{task_id}` |
| POST | `/api/v1/tasks/{task_id}/cancel` |

## 4. 迁移表（旧 → 新；旧路径为弃用别名，暂未定移除版本）

| 旧路径（弃用别名） | Canonical | 归属文件 |
|---|---|---|
| `GET /api/models/{module_id}` | `GET /api/modules/{module_id}/models` | models.rs |
| `POST /api/models/{module_id}/import` | `POST /api/modules/{module_id}/models/import` | models.rs |
| `POST /api/models/{module_id}/download` | `POST /api/modules/{module_id}/models/download` | models.rs |
| `POST /api/models/{module_id}/upload` | `POST /api/modules/{module_id}/models/upload` | upload.rs |
| `POST /api/models/{m}/{model_id}/check-update` | `POST /api/modules/{m}/models/{model_id}/check-update` | models.rs |
| `PUT /api/models/{m}/{model_id}/tags` | `PUT /api/modules/{m}/models/{model_id}/tags` | models.rs |
| `POST /api/models/{m}/{model_id}/cancel-download` | `POST /api/modules/{m}/models/{model_id}/cancel-download` | models.rs |
| `DELETE /api/models/{m}/{model_id}` | `DELETE /api/modules/{m}/models/{model_id}` | models.rs |
| `PUT /api/models/{m}/{model_id}/variant` | `PUT /api/modules/{m}/models/{model_id}/variant` | modules.rs |
| `POST /api/upload/input` | `POST /api/files` | upload.rs |
| `GET /api/modules/export/{module_id}` | `GET /api/modules/{module_id}/export` | module_import.rs |
| `GET\|POST\|DELETE /api/modules/{id}/extra/{*path}` | `.../proxy/{*path}` | module_proxy.rs |
| `POST /api/pipelines/execute` | `POST /api/pipelines/runs`（集合级创建；按 id 执行用 `POST /api/pipelines/{pipeline_id}/runs`） | execute.rs（别名+集合级）/ pipelines.rs（{id} 糖） |
| `GET /api/pipelines/{id}/tasks` | `GET /api/pipelines/{pipeline_id}/runs` | pipelines.rs |
| `POST /api/execute/single` | `POST /api/modules/{module_id}/runs` | execute.rs（别名）/ modules.rs（canonical） |
| `GET /api/v1/inference/result/{task_id}` | `GET /api/v1/tasks/{task_id}` | inference.rs |

路由注册层面由 `api::tests::route_compat` 双向守护：canonical 与别名均不得
落入 `/api` fallback。

## 5. 与 OpenAI 兼容 API 的关系（FAQ）

`/api/v1` 的 `/v1` 是**版本前缀惯例**，不是 OpenAI 兼容契约：本服务不存在
`/v1/chat/completions` 等 OpenAI schema 端点，OpenAI SDK 不能直接指向
EntryPoint。本契约的形状（提交 → 轮询任务 → 相对 URL 取产物）对齐的是
Replicate predictions / fal queue 的异步任务模式。若未来需要让外部 LLM
工具直连本平台的对话类模块，应像 Ollama/vLLM 那样**另挂一层**真正的
OpenAI 兼容端点（独立增量契约，单独文档化），与本门面互不影响。
