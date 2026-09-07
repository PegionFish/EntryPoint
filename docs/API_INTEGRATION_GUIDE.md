# API 接入指南（供应商中立集成模式）

> 面向读者：想把本地 AI 能力（ASR / TTS / OCR / 图像与视频处理等）接入自己
> 工具链的开发者，以及维护多个推理后端接入的集成方。
>
> 本指南描述的接入模式提炼自对主流 AI 内容处理平台的公开 API 的横向调研
> （Replicate、fal.ai、OpenAI、Stability、ElevenLabs、AssemblyAI、Deepgram、
> Runway、Luma、ComfyUI、InvokeAI、n8n、Airflow 等），是这些平台已经收敛出
> 的**行业公共模式**，而不是 EntryPoint 私有约定。EntryPoint 是该模式的
> 一个本地化实现；按本指南接入的工具，核心业务代码不与 EntryPoint 绑定，
> 更换或并存其他遵循同一模式的平台时，只需替换一层薄适配器。

---

## 1. 供应商中立接入模型：五个行业公约数

调研的十余家平台在"媒体/推理类 API"上高度收敛为以下五点，它们构成本指南
的中立接入模型。接入方只依赖这五点，即天然具备可迁移性：

| # | 公约数 | 说明 | 遵循者（示例） |
|---|---|---|---|
| 1 | **`/v1` 版本化根路径** | 版本只放路径根部一级；破坏性变更出 `/v2`，`/v1` 保持稳定 | OpenAI、Replicate、ElevenLabs、Deepgram、Runway、Stripe |
| 2 | **任务即资源**（submit → poll → cancel） | `POST` 一个集合创建异步任务返回 `id`；`GET /{collection}/{id}` 轮询状态并带回产物；`POST /{id}/cancel` 取消。轮询与取结果是**同一个** GET | Replicate `predictions`、OpenAI `batches`/`videos`、Runway `tasks`、Luma `generations`、AssemblyAI `transcript`、fal `requests` |
| 3 | **产物相对 URL / 托管 URL** | 响应只返回可拼接的相对路径或对象存储 URL，绝不回传服务器文件系统绝对路径 | Replicate、fal、Runway、Luma |
| 4 | **稳定错误信封 + 机读码** | 错误体含机器可读码与人类可读文案；客户端逻辑只依赖码，不解析文案 | 本模型约定为 `{"error":{"code","message"}}` |
| 5 | **上传换引用** | 大文件先 `POST` 到文件端点换取引用（id 或 URL），任务提交时携带引用；小输入可直接内联 | OpenAI `files`、AssemblyAI `upload`、Replicate `files` |

不属于中立面的东西（各家的差异化扩展）——能力目录形态、状态词表细节、
回调、WebSocket 推送等——应隔离在适配器里（§7）。

---

## 2. 接入抽象：五操作 Provider 接口

任何遵循上述公约数的平台，都可以映射到同一个最小接口。建议接入方在代码中
先定义这个抽象，再为每个供应商写实现：

```python
class InferenceProvider(Protocol):
    def capabilities(self) -> list[Capability]: ...          # 能力发现
    def submit(self, capability: str, input: Input,
               params: dict | None = None) -> str: ...       # → task_id
    def result(self, task_id: str) -> TaskResult: ...        # 状态 + 产物 URL
    def cancel(self, task_id: str) -> None: ...
    def upload(self, file: BinaryIO) -> str: ...             # → 输入引用
```

`TaskResult` 中的状态建议在适配层归一化为你自己的词表（§6），产物一律持有
URL 句柄而非路径字符串。

---

## 3. EntryPoint 对该模型的实现（适配器映射表）

EntryPoint 的对外稳定契约挂载于 **`/api/v1/*`**（token 鉴权仅作用于此命名
空间，WebUI 内部端点不受影响）。中立五操作与 EntryPoint 端点的对应关系：

| 中立操作 | EntryPoint 端点 | 备注 |
|---|---|---|
| capabilities | `GET /api/v1/capabilities` | 全部模块能力目录，纯只读 |
| submit | `POST /api/v1/inference/{module_id}/{capability}` | 文件走 multipart `file` 字段；文本/引用走 JSON 体；**202** + `{"task_id"}` |
| result | `GET /api/v1/tasks/{task_id}` | 状态 + 产物相对 URL；终态后 `outputs[].url` 可直接下载 |
| list（扩展） | `GET /api/v1/tasks` | `{"tasks":[{task_id,status}]}` 最小稳定面 |
| cancel | `POST /api/v1/tasks/{task_id}/cancel` | 排队中立即终结；运行中置 `cancelled` |
| upload | `POST /api/files` | multipart 单字段 `file` → `{"path": "<引用>"}`；引用传给 submit 的 `input_path` |
| （管线扩展） | `POST /api/pipelines/{pipeline_id}/runs` | 多节点 DAG 执行；同样返回 task_id，后续查询/取消复用 tasks 端点 |

> 历史别名：`GET /api/v1/inference/result/{task_id}`（= `GET
> /api/v1/tasks/{task_id}`）与 `POST /api/upload/input`（= `POST /api/files`）
> 保留为弃用别名，新接入请使用 canonical 路径。完整迁移表见
> [API_DESIGN.md](API_DESIGN.md)。

与其他平台的一一对照（接入方做适配器时的参考映射）：

| 语义 | EntryPoint | Replicate | fal.ai | OpenAI |
|---|---|---|---|---|
| 提交 | `POST /v1/inference/{m}/{c}` | `POST /v1/predictions` | `POST /{model_id}` | `POST /v1/batches` |
| 轮询+产物 | `GET /v1/tasks/{id}` | `GET /v1/predictions/{id}` | `GET .../requests/{id}` | `GET /v1/batches/{id}` |
| 取消 | `POST /v1/tasks/{id}/cancel` | `POST /v1/predictions/{id}/cancel` | `PUT .../cancel` | `POST /v1/batches/{id}/cancel` |
| 上传 | `POST /files` | `POST /v1/files` | （无，走外部 URL） | `POST /v1/files` |

---

## 4. 认证

服务端在 `config/app.toml` 配置 `api.token` 后，`/api/v1/*` 全部要求：

```http
Authorization: Bearer <token>
# 或
X-API-Key: <token>
```

- 未配置 token 或 `api.enabled = false` 时直通（本地信任环境）；
- 两种头任选其一，服务端常量时间比较；
- 401 响应同样是标准信封：`{"error":{"code":"UNAUTHORIZED","message":...}}`；
- 产物下载 URL（`/api/tasks/{task_id}/artifacts/{node_id}`）不在 v1 命名空间
  内、不受 token 保护——这是有意设计：产物 URL 是自包含的短期句柄，适配器
  无需为下载补签头。若你的部署要求下载也鉴权，请在前置网关层统一加。

---

## 5. 端到端接入流程（以 EntryPoint 为例）

### 5.1 能力发现

```bash
curl -s -H "Authorization: Bearer $EP_TOKEN" \
  "$BASE/api/v1/capabilities"
```

```json
{
  "modules": [
    {
      "module_id": "whisper-asr",
      "capabilities": [
        { "id": "transcribe",
          "params": [ { "name": "language", "type": "string", "required": false } ] }
      ]
    }
  ]
}
```

建议：接入方在启动时拉取一次并缓存，`MODULE_NOT_FOUND` /
`CAPABILITY_NOT_FOUND` 错误时刷新重试（模块可被动态安装/卸载）。

### 5.2 提交任务

三种输入形态，按场景选择：

**a. 纯文本输入（JSON 体）**

```bash
curl -s -X POST -H "Authorization: Bearer $EP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"input_text": "要处理的文本", "params": {"language": "zh"}}' \
  "$BASE/api/v1/inference/qwen3-tts/tts"
```

**b. 小文件直接内联（multipart 体）**

```bash
curl -s -X POST -H "Authorization: Bearer $EP_TOKEN" \
  -F "file=@audio.wav" \
  -F 'params={"language":"auto"}' \
  "$BASE/api/v1/inference/whisper-asr/transcribe"
```

**c. 大文件先上传换引用（推荐，可复用/断点友好）**

```bash
REF=$(curl -s -X POST -H "Authorization: Bearer $EP_TOKEN" \
  -F "file=@video.mp4" "$BASE/api/files" | jq -r .path)

curl -s -X POST -H "Authorization: Bearer $EP_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"input_path\": \"$REF\"}" \
  "$BASE/api/v1/inference/video-upscale/upscale"
```

三种形态的成功响应统一为 **202** + `{"task_id": "<id>"}`。
（`input_text` 与 `input_path` 互斥，同传/双缺 → 400 `INPUT_INVALID`。）

### 5.3 轮询（含推荐退避策略）

```bash
curl -s -H "Authorization: Bearer $EP_TOKEN" "$BASE/api/v1/tasks/$TASK_ID"
```

```json
{
  "task_id": "b3f2…",
  "status": "completed",
  "outputs": [
    { "node_id": "tts", "url": "/api/tasks/b3f2…/artifacts/tts" }
  ]
}
```

- 推荐间隔：首次 1s，指数退避（×1.5），5s 封顶；排队信息见
  `queue_position`（存在时）；
- 终态判定只依赖 §6 的归一化状态，不解析 `status` 原文之外的字段；
- `error` 字段仅在失败时出现，人类可读，**不要**拿它做机判。

### 5.4 下载产物

`outputs[].url` 是相对 URL，拼上 base 即可下载：

```bash
curl -s -o result.wav "$BASE/api/tasks/b3f2…/artifacts/tts"
```

产物 URL 应视为**不透明句柄**：不要解析其内部结构、不要持久化拼接规则，
唯一合法操作是"base + url 然后 GET"。

### 5.5 取消与超时

```bash
curl -s -X POST -H "Authorization: Bearer $EP_TOKEN" \
  "$BASE/api/v1/tasks/$TASK_ID/cancel"
```

- 已取消 → 200 `{"task_id":..., "status":"cancelled"}`；
- 已是终态 → 409 `TASK_ALREADY_TERMINAL`（幂等语义：把它当"已结束"处理即可）；
- 不存在 → 404 `TASK_NOT_FOUND`。

接入方务必设置**整体业务超时**（如 30 分钟），超时后先 cancel 再标记失败，
避免依赖服务端清理节奏。

### 5.6 完整 Python 适配器示例

```python
import time
import requests

class EntryPointProvider:
    """中立 Provider 接口的 EntryPoint 实现。换供应商 = 换这个类。"""

    def __init__(self, base_url: str, token: str | None = None):
        self.base = base_url.rstrip("/")
        self.s = requests.Session()
        if token:
            self.s.headers["Authorization"] = f"Bearer {token}"

    # ── capabilities ──
    def capabilities(self) -> dict:
        return self._get("/api/v1/capabilities")

    # ── upload（大文件换引用）──
    def upload(self, fileobj) -> str:
        return self._post_files("/api/files", files={"file": fileobj})["path"]

    # ── submit ──
    def submit(self, module_id: str, capability: str,
               *, input_text: str | None = None,
               input_path: str | None = None,
               fileobj=None, params: dict | None = None) -> str:
        url = f"/api/v1/inference/{module_id}/{capability}"
        if fileobj is not None:
            data = {"params": __import__("json").dumps(params or {})}
            r = self.s.post(self.base + url, data=data,
                            files={"file": fileobj}, timeout=300)
        else:
            body = {"params": params or {}}
            if input_text: body["input_text"] = input_text
            if input_path: body["input_path"] = input_path
            r = self.s.post(self.base + url, json=body, timeout=300)
        self._check(r)
        return r.json()["task_id"]

    # ── result ──
    def result(self, task_id: str) -> dict:
        return self._get(f"/api/v1/tasks/{task_id}")

    # ── cancel ──
    def cancel(self, task_id: str) -> None:
        self.s.post(self.base + f"/api/v1/tasks/{task_id}/cancel", timeout=30)

    # ── 轮询辅助（含归一化，见 §6）──
    def wait(self, task_id: str, timeout_s: float = 1800) -> dict:
        deadline, delay = time.monotonic() + timeout_s, 1.0
        while time.monotonic() < deadline:
            r = self.result(task_id)
            state = NORMALIZE.get(r["status"], "unknown")
            if state in ("succeeded", "failed", "cancelled"):
                return r
            time.sleep(delay)
            delay = min(delay * 1.5, 5.0)
        self.cancel(task_id)
        raise TimeoutError(task_id)

    # ── 内部工具 ──
    def _get(self, path):
        r = self.s.get(self.base + path, timeout=30)
        self._check(r)
        return r.json()

    def _post_files(self, path, **kw):
        r = self.s.post(self.base + path, **kw, timeout=3600)
        self._check(r)
        return r.json()

    @staticmethod
    def _check(r):
        if r.status_code >= 400:
            # 只依赖机读码，不解析 message 文案
            code = r.json().get("error", {}).get("code", f"HTTP_{r.status_code}")
            raise ProviderError(code, r.status_code)

class ProviderError(Exception):
    def __init__(self, code, status):
        super().__init__(f"{code} (HTTP {status})")
        self.code, self.status = code, status
```

---

## 6. 状态词表归一化（可移植性的关键）

各家状态原文不同，适配器必须把它们映射到你自己的词表。EntryPoint 的状态
词表为：`queued / running / completed / failed / cancelled`
（后三者为终态）：

| 归一化 | EntryPoint | Replicate | fal | OpenAI batches | Runway |
|---|---|---|---|---|---|
| 排队 | `queued` | `starting` | `IN_QUEUE` | `validating`/`finalizing` | `PENDING` |
| 运行 | `running` | `processing` | `IN_PROGRESS` | `in_progress` | `PENDING`/`THROTTLED` |
| 成功 | `completed` | `succeeded` | `COMPLETED` | `completed` | `SUCCEEDED` |
| 失败 | `failed` | `failed` | —（HTTP 错误） | `failed`/`expired` | `FAILED` |
| 取消 | `cancelled` | `canceled` | — | `cancelling`/`cancelled` | `CANCELLED` |

Python 侧一行归一化（本指南示例所用）：

```python
NORMALIZE = {"queued": "queued", "running": "running",
             "completed": "succeeded", "failed": "failed",
             "cancelled": "cancelled"}
```

---

## 7. 差异化扩展的隔离原则

EntryPoint 提供的部分能力**不在中立面上**，接入方可选用，但必须作为适配器
的可选扩展实现，核心流程不得依赖：

| 扩展 | 端点/机制 | 中立替代 |
|---|---|---|
| 同步等待 | submit/runs 的 `wait=true`（阻塞至终态，响应带 `output_url`） | 标准轮询循环 |
| 终态回调 | 提交时带 `callback_url`，终态时服务端 POST `{task_id, status, artifacts}` | 轮询 |
| 多节点管线 | `POST /api/pipelines/{pipeline_id}/runs` | 拆成多次单能力 submit 自行编排 |
| 实时进度 | WebSocket `/ws`、`/ws/logs`、`/ws/progress`（内部面，无版本契约） | 轮询 |
| 能力参数 schema | `capabilities` 响应内 `params` 数组 | 固定参数表 |

原则：**用扩展可以加速，用中立面兜底**——扩展不可用时，接入方必须能退化到
轮询 + 单能力提交仍完整工作。

---

## 8. 错误契约与机读码表

`/api/v1/*` 所有错误统一为：

```json
{ "error": { "code": "MACHINE_CODE", "message": "人类可读文案（勿机判）" } }
```

| code | HTTP | 触发场景 | 接入方建议处理 |
|---|---|---|---|
| `UNAUTHORIZED` | 401 | token 缺失/错误 | 检查凭据配置 |
| `PARAM_INVALID` | 400 | params 与能力 schema 不符 | 修正参数后重试 |
| `INPUT_INVALID` | 400 | input_text/input_path 双缺或同传、文件缺失 | 修正输入 |
| `MODULE_NOT_FOUND` | 404 | module_id 不存在/未安装 | 刷新 capabilities |
| `CAPABILITY_NOT_FOUND` | 404 | capability 不存在 | 刷新 capabilities |
| `MODEL_NOT_READY` | 409 | 模型变体未下载就绪 | 触发下载或提示用户 |
| `MODULE_START_FAILED` | 500 | 模块自动拉起失败 | 查模块日志，退避重试 |
| `QUEUE_FULL` | 429 | 并发闸已满 | 退避重试（当前实现有排队闸，见 429） |
| `TASK_NOT_FOUND` | 404 | task_id 不存在 | 视为永久失败 |
| `TASK_ALREADY_TERMINAL` | 409 | cancel 打在终态任务上 | 当"已结束"处理 |
| `INTERNAL` | 5xx | 服务端内部错误 | 退避重试，持续失败再上报 |

文案语言跟随服务端语言配置且可能本地化，**客户端逻辑只允许依赖 `code`**。

---

## 9. 可移植性检查清单

接入代码合并前，对照以下清单自查（全部满足才算"供应商中立"）：

- [ ] base URL、token、超时全部来自配置注入，代码中无硬编码地址；
- [ ] 提交/轮询/取消/上传五个操作各自只有"一个适配器类"知道供应商细节；
- [ ] 状态机只依赖归一化词表，不出现 `queued` 等原文于核心代码；
- [ ] 产物 URL 只作不透明句柄使用，无路径解析、无持久化拼接规则；
- [ ] 错误处理只 switch 机读码，不解析 message 文案（文案可能随语言变）；
- [ ] 未依赖任何"扩展面"（wait/callback/WS/管线）作为唯一路径，或有退化方案；
- [ ] 对 429 与 5xx 实现指数退避重试；
- [ ] 有整体业务超时，超时路径先 cancel。

---

## 10. 当前边界与演进声明

诚实地列出本契约**当前不是**什么，避免接入方误期望建立在未定义行为上：

1. **不是 OpenAI 兼容 API。** `/api/v1` 的 `/v1` 仅为版本前缀（行业惯例，
   与 Stripe/Replicate 同款）；不存在 `/v1/chat/completions` 等 OpenAI
   schema 端点，OpenAI SDK 无法直接指向 EntryPoint。若未来为对话类模块
   提供 OpenAI 兼容层（如 Ollama/vLLM 那样另挂 `/v1/chat/completions`），
   将作为独立增量契约单独文档化，与本门面互不影响。
2. **`GET /v1/tasks` 当前无分页。** 任务为内存注册表、重启即清空（持久化在
   `workspace/tasks/` 目录，但 API 列表不回读磁盘）；需要持久任务历史的接入
   方请自行落盘。
3. **上传引用 `path` 为服务器本地路径形态。** 它是语义上的"输入引用"，只允许
   指向 uploads 区（服务端有前缀校验与穿越防护）；请勿解析、改写或持久化
   其字符串内容。
4. **WebSocket 进度通道不在稳定契约内。** `/ws*` 为 WebUI 内部面，消息形状
   可能随版本调整；外部集成请用轮询。

---

## 附：相关文档

- [INFERENCE_API.md](INFERENCE_API.md) — `/api/v1` 契约的权威字段级定义
- [API_DESIGN.md](API_DESIGN.md) — 端点设计原则、完整端点表与迁移/弃用策略
- [AUTOMATION.md](AUTOMATION.md) — 面向本机自动化脚本的场景化指南
- [ADAPTER_API.md](ADAPTER_API.md) — 模块（推理服务）侧接入规范
