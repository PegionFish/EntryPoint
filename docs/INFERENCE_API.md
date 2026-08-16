# 统一推理 API v1（`/api/v1/*`）

面向外部系统（自动化集成 / 第三方调用方）的**稳定推理门面**。与 `/api` 其余
WebUI 内部端点分离：本文件描述的请求/响应形状与错误码属对外契约，变更走
版本演进（v2 新前缀，不破坏 v1）；WebUI 内部端点的 i18n 本地化错误文案
不适用于本门面。

> 契约分层见 `docs/AUTOMATION.md` §5：`/api/v1/*` = 外部稳定契约；
> `/api/*` 其余 = WebUI 内部契约（集成方不应依赖其形状）。

---

## 1. 端点清单

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/v1/capabilities` | 能力目录：聚合所有已安装模块 manifest 的 capability 声明（纯只读，永不调用模块进程） |
| `POST` | `/api/v1/inference/{module_id}/{capability}` | 提交推理（multipart 文件或 JSON 两种形态） |
| `GET` | `/api/v1/inference/result/{task_id}` | 查询任务状态与产物（轮询用） |

### 1.1 `GET /v1/capabilities`

返回形状：

```json
{
  "capabilities": [
    {
      "module_id": "faster-whisper",
      "capability": "transcribe",
      "description": "...",
      "input_type": "file",
      "output_type": "file",
      "max_file_size_mb": 512,
      "params": { "language": { "type": "string" } }
    }
  ]
}
```

无模块时返回 `{"capabilities": []}`（200，不报错）。

### 1.2 `POST /v1/inference/{module_id}/{capability}`

两种输入形态（按 `Content-Type` 分流）：

**A. multipart/form-data**（文件输入）：

| 字段 | 必需 | 说明 |
|---|---|---|
| `file` | 是 | 输入文件（落盘至 workspace/uploads）；大小受能力声明 `max_file_size_mb` 限制（未声明时 2GB 兜底），超限 → `400 INPUT_INVALID` |
| `params` | 否 | JSON 字符串（对象），模块参数；字段上限 1MB |
| `wait` | 否 | `"true"` 同步等待终态；缺省异步；字段上限 1MB |

重复 `file` 字段以最后一个为准（先前已接收的文件会被清理）。

**B. application/json**（文本或已上传文件）：

```json
{
  "input_text": "纯文本输入（与 input_path 二选一，互斥）",
  "input_path": "仅限 workspace/uploads 前缀的路径",
  "params": {},
  "wait": false,
  "callback_url": "http://your-host/done"
}
```

`input_text` 与 `input_path` **互斥**：双缺或同传均返回 `400 INPUT_INVALID`
（对齐 `POST /api/execute/single` 双字段 400 口径）。JSON 请求体上限
8MB（推理提交的文本/params 为 KB 量级，超限即错，防无上限缓冲）。

安全约束：`input_path` 经服务端 canonicalize 后必须位于 workspace/uploads
之内（防符号链接 / `..` 穿越），**绝不透传任意服务器绝对路径**；越权返回
`400 INPUT_INVALID`。跨机器输入一律走 multipart 上传或先调
`POST /api/upload/input`。

**响应**：

- 异步（`wait` 缺省/false）→ `202`：

  ```json
  { "task_id": "task-...", "queue_position": 2 }
  ```

  `queue_position` 仅在提交时任务排队才出现（从 1 起）。

- 同步（`wait=true`）→ `200`，阻塞至任务终态：

  ```json
  {
    "task_id": "task-...",
    "status": "completed",
    "output_url": "/api/tasks/task-.../artifacts/output",
    "error": "仅失败时携带"
  }
  ```

  `output_url` 为**相对下载 URL**（302 到文件流通道），绝不返回服务器
  绝对路径；output 节点产物优先。

服务端执行序列（对齐 `POST /api/execute/single`）：manifest/capability
存在性 → 参数 schema 校验与默认值注入 → 输入文件存在性 → 模块自动拉起
（未运行则启动并等健康）→ 提交执行引擎。

### 1.3 `GET /v1/inference/result/{task_id}`

```json
{
  "task_id": "task-...",
  "status": "queued | running | completed | failed | cancelled",
  "outputs": [
    { "node_id": "output", "url": "/api/tasks/task-.../artifacts/output" }
  ],
  "queue_position": 1,
  "error": "仅失败时携带"
}
```

`outputs` 一律相对下载 URL；`queue_position` 仅排队时实时携带。未知
task → `404 TASK_NOT_FOUND`。

---

## 2. 错误契约

所有 v1 端点错误体形状稳定（不含 i18n 文案）：

```json
{ "error": { "code": "MACHINE_CODE", "message": "可读文案" } }
```

| code | HTTP | 含义 |
|---|---|---|
| `UNAUTHORIZED` | 401 | token 缺失或不匹配（配置了 `[api].token` 时） |
| `MODULE_NOT_FOUND` | 404 | 模块未安装/未发现 |
| `CAPABILITY_NOT_FOUND` | 404 | 模块无此 capability |
| `TASK_NOT_FOUND` | 404 | 结果查询的任务不存在 |
| `PARAM_INVALID` | 400 | 参数缺失/类型不符/枚举越界 |
| `INPUT_INVALID` | 400 | 输入缺失、`input_path` 越权、multipart/JSON 形态非法；亦含 `input_text`/`input_path` 同传（互斥）、输入文件超出 `max_file_size_mb` 声明上限、multipart 文本字段超 1MB、JSON 请求体超 8MB |
| `MODEL_NOT_READY` | 409 | 激活变体模型未下载就绪 |
| `MODULE_START_FAILED` | 502 | 模块自动拉起失败（启动错误/健康超时等） |
| `QUEUE_FULL` | 429 | 任务队列已满 |
| `INTERNAL` | 500 | 兜底内部错误 |

---

## 3. 队列与并发语义（吞吐模型）

如实说明当前吞吐模型，便于调用方做容量规划：

- **全局闸门**：所有任务（含 v1 推理）共享 `[pipeline].max_parallel` 并发
  上限，超额自动 `queued` 排队（FIFO），无需客户端限流；队列满时提交返回
  `429 QUEUE_FULL`。
- **模块内串行**：单个模块 adapter 为单 worker 串行处理请求——同一模块的
  多个推理任务即使都获准运行，也在 adapter 层逐个执行。吞吐提升依赖
  多模块/多 capability 并行或后续多实例演进。
- 提交即排队：202 响应后任务可能在 `queued` 停留较久（取决于在跑任务），
  轮询间隔建议 ≥1s。

## 4. 同步 / 异步选择建议

- **短任务**（秒级，如小图处理/短文本）：`wait=true` 一次往返拿结果；
  v1 推理**提交**端点已豁免 daemon 300s 请求超时，阻塞上限由任务引擎
  管理；结果查询端点（`GET /v1/inference/result/{task_id}`）为快速读，
  不豁免，仍受 300s 兜底约束。
- **长任务**（分钟级，如视频转写）：**异步 + `callback_url`** 或轮询
  result 端点。`wait=true` 会占住一个 HTTP 连接直到终态，长任务下代理/
  负载均衡器可能先行断连；回调为 best-effort（终态时 POST
  `{task_id, status, artifacts}` 到你的 URL），回调与轮询可并用。

---

## 5. curl 示例

```bash
BASE=http://localhost:9800

# 0) 能力发现
curl $BASE/api/v1/capabilities

# 1) multipart 文件调用（异步）
curl -X POST $BASE/api/v1/inference/faster-whisper/transcribe \
  -F 'file=@./audio.wav' \
  -F 'params={"language":"zh"}'
# → 202 {"task_id":"task-..."}

# 2) JSON 文本调用（异步）
curl -X POST $BASE/api/v1/inference/qwen3-tts/synthesize \
  -H 'Content-Type: application/json' \
  -d '{"input_text":"你好，世界","params":{"voice":"default"}}'

# 3) wait 同步（阻塞至终态）
curl -X POST $BASE/api/v1/inference/faster-whisper/transcribe \
  -F 'file=@./audio.wav' -F 'wait=true'
# → 200 {"task_id":"...","status":"completed","output_url":"/api/tasks/.../artifacts/output"}

# 4) 异步轮询结果
curl $BASE/api/v1/inference/result/<task_id>
# 产物下载（相对 URL → 302 → 文件流）：
curl -OJ $BASE/api/tasks/<task_id>/artifacts/output

# 5) 异步 + 完成回调
curl -X POST $BASE/api/v1/inference/faster-whisper/transcribe \
  -H 'Content-Type: application/json' \
  -d '{"input_path":"<workspace/uploads 内的路径>","callback_url":"http://localhost:8080/done"}'

# 6) 携带 token（配置了 [api].token 时必需）
curl $BASE/api/v1/capabilities -H 'Authorization: Bearer <token>'
# 或：-H 'X-API-Key: <token>'
```

---

## 6. token 配置方法

`config/app.toml`：

```toml
[api]
enabled = true          # 缺省 true；false 时 v1 端点无鉴权要求
token = "长随机串"      # 缺省不设 = 直通（与历史行为一致）
```

- 配置 token 后，v1 端点要求 `Authorization: Bearer <token>` 或
  `X-API-Key: <token>`（常量时间比较），不匹配返回 `401 UNAUTHORIZED`；
- token **仅保护 `/api/v1/*`**，不影响 WebUI 内部端点；
- `[server] allow_public = true` 且未配 token 时，daemon 启动日志打印
  未认证公网暴露风险告警——公网部署必须同时配置 token，并在前置反向代理
  层补强防护（详见 `docs/AUTOMATION.md` §5）。

完整配置项参考 `docs/CONFIG_REFERENCE.md` §1.11。

---

## 7. 演进路线

本期（一期）交付：能力目录 + 双形态提交 + 结果查询 + 可选 token 鉴权 +
稳定错误契约。以下为已规划但**尚未实现**的后续方向（契约面保持兼容）：

- **二期**：模块闸门（推理级并发准入）、keep_warm（模块常驻）、任务 TTL
  （结果与产物过期清理）；
- **三期**：大文件路径传递（免上传直读）、idle 自动停止（省显存）、
  OpenAI 兼容别名端点、模块多实例（吞吐扩展）。
