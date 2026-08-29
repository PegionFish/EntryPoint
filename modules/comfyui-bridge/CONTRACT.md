# ComfyUI 桥接模块 — 第 0 轮冻结契约（F1）

> 本文件是所有并行子代理的唯一编程依据。任何变更须经编排者仲裁并广播。
> 依据：docs/COMFYUI_BRIDGE_PLAN.md §3（冻结）+ 本文件补充的签名表/行为矩阵/错误映射表。

---

## 1. 模块清单契约（module.toml 形状）

```toml
[module]
id = "comfyui-bridge"
name = "ComfyUI 桥接"
category = "image"
genre = "comfyui"

[runtime]
type = "python"
entrypoint = "adapter.py"
start_command = "{venv_python} {MODULE_DIR}/{entrypoint}"

[compute]
backends = ["cpu"]          # 代理本身不做计算
default_backend = "cpu"

[compute.env]
cpu = { COMFYUI_URL = "http://127.0.0.1:8188" }

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 60

[[interface.capabilities]]
name = "generate"
description = "向 ComfyUI 提交工作流并取回产物"
input_type = "file"
output_type = "file"

[interface.capabilities.params]
workflow     = { type = "string", description = "已上传工作流名（不含 .json）" }
inject       = { type = "string", description = "注入映射 JSON，语法见 README" }
base_url     = { type = "string", description = "覆盖 COMFYUI_URL（远程实例）" }
output_nodes = { type = "string", description = "取回输出的 Save 节点 id，逗号分隔；缺省取全部" }
```

要点：
- **不声明 `[[models]]`**（桥接模块无权重）。
- `requirements.txt` 仅 4 行（与现役模块同款版本线，与 config/constraints.txt 无冲突）：
  ```
  fastapi>=0.100.0
  uvicorn[standard]>=0.23.0
  python-multipart>=0.0.6
  httpx>=0.27.0
  ```

## 2. Adapter HTTP 契约

### 2.1 平台标准端点（遵守 docs/ADAPTER_API.md）

| 端点 | 行为 |
|---|---|
| `GET /health` | 代理探测 ComfyUI `GET /system_stats`，通 → 200 `{"status":"ok","comfyui":"reachable"}`；不通 → **503** `{"status":"unavailable","comfyui":"unreachable"}`（使"管线执行前自动拉起"失败语义直白） |
| `GET /info` | 200 `{"module":"comfyui-bridge","version":"0.1.0","comfyui":{"reachable":bool,"version":str|null},"queue":{"running":int,"pending":int}}` |
| `POST /predict/generate` | 同步阻塞全流程（见 2.3）；成功 200 `{"status":"completed","output_type":"file","result":"<主产物绝对路径>"}`；失败按 §4 错误映射返回 4xx/5xx `{"error": "<message>"}` |

客户端断开时尽力 `POST /interrupt`（best-effort，文档诚实声明）。

### 2.2 工作流管理端点

| 端点 | 行为 |
|---|---|
| `GET /workflows` | 200 `[{"name":str,"size_bytes":int,"mtime":int}]`（mtime 为 epoch 秒），按名字排序 |
| `POST /workflows` | multipart `file` 字段；校验：合法 JSON + API 格式（顶层对象、每个值是对象且含 `class_type` 与 `inputs`）→ 落盘 `workflows/<清洗后文件名>.json`；重名覆盖；200 `{"name":str,"replaced":bool}`；非法 → 400 `{"error":...}` |
| `DELETE /workflows/{name}` | 200 `{"ok":true}`；不存在 → 404 |

文件名清洗：仅保留 `[A-Za-z0-9._-]`，剥路径分量（防穿越），剥 `.json` 后缀后重建；清洗后为空 → 400。

### 2.3 generate 全流程

1. 接收 multipart 上游产物（字段名 = 上游节点 id；单输入时也接受字段名 `file`/`input`）；文本类上游（`.txt`）作为文本注入候选。
2. 解析 `inject` 参数（JSON 对象，语法见 §3）；**注入前逐项校验键存在性**——键 = `<工作流节点id>.inputs.<字段名>`，节点 id 不在工作流中 → 400 并列出模板可用节点清单（D6）。缺失字段允许（写入新键）。
3. 文件类来源（`$input*` 指向文件）先 `POST /upload/image`，把返回的 `name` 写入目标字段；文本/字面量原样写入。
4. `POST /prompt` 提交；`prompt_id` 失败 → 502。
5. 轮询 `GET /history/{prompt_id}`（间隔 1s 起、指数退避至 5s 封顶；每轮打印 `EP-PROGRESS:NN%`，按已完成输出节点数/预估总输出数估算，无输出时按 `queue` 状态给 5%~95% 心跳）。
6. 完成后按 `output_nodes`（逗号分隔 Save 节点 id；缺省取全部有 images 的节点）经 `GET /view?filename=&type=output` 下载全部产物到注入的 `output_path` 目录；**第一个为主产物**。
7. 返回 `{"status":"completed","output_type":"file","result":"<主产物绝对路径>"}`。

### 2.4 环境变量与目录约定

- `EP_PORT`：监听端口；`HOST`：绑定地址（默认 127.0.0.1）。
- `COMFYUI_URL`：默认 ComfyUI 地址；参数 `base_url` 优先。
- `MODULE_DIR`：模块目录（workflows/ 与 output 目录解析基准）；`EP_OUTPUT_DIR` 或 params `output_path`：产物落盘目录（平台引擎注入；两者皆缺 → `MODULE_DIR/output/`）。

## 3. inject 映射语法（D5）

`inject` 为 JSON 对象，键 = `<工作流节点id>.inputs.<字段名>`，值 = 来源表达式：

| 表达式 | 语义 |
|---|---|
| `$input` | 首个上游产物文件 |
| `$input.<上游节点id>` | 定向引用指定上游节点的文件产物（多条输入边时必需） |
| `$input.<上游节点id>`（上游为文本产物） | 文本注入字符串字段（txt2img 提示词场景） |
| 字面量 | 数字/字符串/布尔常量（seed、steps 等） |

执行规则：
1. 文件类来源先 `POST /upload/image` 上传、再把返回文件名写入字段；文本/字面量原样写入。
2. **注入前逐项校验键存在**（节点 id 必须在工作流中）；缺失立即 400 报错并列出可用节点清单，不提交给 ComfyUI（D6）。
3. 未映射字段保留模板默认值；键天然唯一，无冲突。
4. `output_nodes` 指定取回哪些输出；全部下载到产物目录，第一个为主产物返回下游。

示例：
```json
{
  "3.inputs.image": "$input",
  "5.inputs.image": "$input.ref",
  "7.inputs.text":  "$input.prompt",
  "9.inputs.seed":  42,
  "9.inputs.steps": 28
}
```

## 4. ComfyUI REST 端点清单与错误映射表

### 4.1 端点（comfy_client 封装范围）

`GET /system_stats` · `POST /upload/image` · `POST /prompt` · `GET /history/{prompt_id}` · `GET /view?filename=&type=output` · `POST /interrupt` · `GET /queue`

### 4.2 错误映射表（ComfyUI 侧现象 → adapter 响应）

| 现象 | HTTP | error 语义（英文技术信息，中文走日志） |
|---|---|---|
| `/system_stats` 不通 | （/health 时）503 | comfyui unreachable |
| inject JSON 非法 / 键不存在 | 400 | invalid inject mapping: <detail> + available nodes |
| workflow 名不存在 | 400 | workflow "<name>" not found; available: [...] |
| `POST /prompt` 400（工作流被 ComfyUI 拒绝） | 502 | comfyui rejected prompt: <node errors 摘要> |
| 轮询超时（默认 1800s，可被引擎节点 timeout 先杀） | 504 | comfyui generation timeout after Ns |
| history 中 status_str=error / messages 含 execution_error | 502 | comfyui execution error: <摘要> |
| `/view` 下载失败 | 502 | failed to fetch output <filename> |
| 产物目录不可写 | 500 | output dir not writable: <path> |

## 5. comfy_client.py 方法签名表（F1 冻结，B 据此桩编程）

```python
class ComfyClientError(Exception):
    """分类：connect / rejected / execution / timeout / output"""

class ComfyClient:
    def __init__(self, base_url: str, timeout: float = 30.0): ...
    async def system_stats(self) -> dict:
        """GET /system_stats；不通抛 ComfyClientError('connect')"""
    async def queue_info(self) -> dict:
        """GET /queue → {'running': int, 'pending': int}"""
    async def upload_image(self, file_bytes: bytes, filename: str) -> str:
        """POST /upload/image (multipart imageoverwrite/type=input) → 返回服务器侧文件名"""
    async def submit_prompt(self, workflow: dict) -> str:
        """POST /prompt {'prompt': workflow} → prompt_id；4xx 抛 'rejected'（含 node_errors 摘要）"""
    async def poll_history(self, prompt_id: str, *, interval0: float = 1.0,
                           interval_max: float = 5.0, timeout: float = 1800.0,
                           on_progress=None) -> dict:
        """轮询 /history/{id} 至终态；on_progress(pct: int) 回调；超时抛 'timeout'；
        返回该 prompt_id 的 history 条目（含 outputs/status）"""
    async def fetch_output(self, filename: str, subfolder: str = "", *,
                           dest_dir: Path) -> Path:
        """GET /view 下载到 dest_dir（保留原名；重名由调用方/下载侧加序号）→ 返回落盘路径"""
    async def interrupt(self) -> None:
        """POST /interrupt，best-effort（吞掉连接错误）"""
```

代理策略：`base_url` 为回环地址（127.0.0.1/localhost）时对 httpx 显式 `trust_env=False`（绕过本机代理）；远程尊重环境代理。

## 6. mock_comfyui 行为矩阵（C 代理实现，A/B 测试依赖）

| 端点 | 行为 |
|---|---|
| `GET /system_stats` | 200 固定 `{"system":{"comfyui_version":"mock-0.3"}}` |
| `GET /queue` | 200 `{"queue_running":[],"queue_pending":[]}`（受错误注入影响见下） |
| `POST /upload/image` | 200 `{"name":"<filename>","subfolder":"","type":"input"}`；记录已上传文件名集合 |
| `POST /prompt` | 校验 body JSON 含 `prompt` 对象 → 生成 `prompt_id`（自增十六进制）→ 200 `{"prompt_id":...,"number":n}`；`prompt` 缺失/非对象 → 400 `{"error":...}`；注入开关 `reject_prompt=1` → 恒 400 |
| `GET /history/{id}` | 第 1..N-1 次 → 200 `{}`（空对象=未完成）；第 N 次 → 200 完整条目：`{"<id>":{"prompt":[...],"outputs":{"<save_node>":{"images":[{"filename":"out_<id>_0.png","subfolder":"","type":"output"}]}},"status":{"status_str":"success","completed":true}}}`；N 由该 prompt 提交时可配置（默认 2）；注入开关 `fail_execution=1` → 第 N 次返回 `status_str="error"` + `messages:[["execution_error",{"node_type":"KSampler","exception_message":"mock boom"}]]` |
| `GET /view` | 200 固定字节 `b"MOCK-PNG-BYTES:" + filename`；参数缺失 → 400 |
| `POST /interrupt` | 200 `{"ok":true}`；记录调用计数（供测试断言取消路径） |

实现：纯标准库 `http.server`（ThreadingHTTPServer），`create_server(host, port, *, history_rounds=2, reject_prompt=False, fail_execution=False)` + `serve_forever`；错误注入经服务器属性可运行期翻转；不依赖 pytest。

## 7. 文件域与回滚

- 回滚单元：整目录 `modules/comfyui-bridge/` 删除 + 还原 `module_proxy.rs`/`api/mod.rs` 路由行/前端 3 处存量改动。
- 禁触（桥接阶段）：`crates/ep-core/**`、`executor.rs`/`dag.rs`/`runner.rs`、`pipeline-node.tsx` 内置节点定义、`config/constraints.txt`。
