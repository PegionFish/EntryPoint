# OpenAI 兼容桥接模块 — 契约（T-23 M1）

> 模块：`modules/openai-bridge/`（独立模块仓；modules/ 在主仓库 gitignore 内）
> 依据：reports/plan/2026-10-25-wave2-dispatch.md T-23 ·
> reports/audit/2026-10-25-master-report.md 计划6补充 ·
> docs/ADAPTER_API.md §8（策略 C）· docs/MODULE_SPEC.md §5/§10。
> 先例：`modules/comfyui-bridge/`（桥接模式与测试模式复制模板）。

---

## 1. module.toml 形状

- `type="python"`、`python_version=">=3.10"`（无 torch 等原生 wheel 依赖，不设上界）
- `backends=["cpu"]`、`default_backend="cpu"`、`ram_estimate_mb=128`（纯代理）
- **无 `[[models]]`**（桥接模块无权重；模型名经 `params.model` 运行时指定）
- `[compute.env] cpu = { OPENAI_BASE_URL = "https://api.openai.com/v1" }` —— **绝不注入密钥**
- 三能力：`chat`（text→text）/ `transcribe`（audio→text）/ `speak`（text→audio）
- `ready_timeout_secs=15`（秒级就绪；上游不可达经 /health 503 直白暴露，私网上游时）

## 2. 三能力 → 上游端点映射（M1 固定）

| 能力 | 上游端点 | 输入 | 输出 | 用途（计划6补充） |
|---|---|---|---|---|
| `chat` | `POST {base_url}/chat/completions` | text（或 SRT） | text（或 SRT 文件） | 管线翻译/润色/脚本节点 |
| `transcribe` | `POST {base_url}/audio/transcriptions`（纯音频）；SRT 输入时经 `/chat/completions` | audio 文件 或 SRT | text / SRT / JSON | 云端 ASR 兜底 |
| `speak` | `POST {base_url}/audio/speech` | text | 音频文件（mp3/wav/…） | 云端 TTS 兜底/对比 |

`embed` / `generate_image` 属计划6补充的可选能力，M1 不实现（边界见 §8）。

### 2.1 端点（adapter 对平台）

| 端点 | 行为 |
|---|---|
| `GET /health` | 上游为**私网/回环**地址 → 代理探测 `GET {base}/models`，任何 HTTP 响应（含 401/404）= 可达 200；连接失败 → **503** `{"status":"unavailable","openai":"unreachable"}`。上游为**公网**地址 → 不探测（健康检查不依赖外网/不耗配额）恒 200，上游可用性在调用时以 502/504 暴露 |
| `GET /info` | `{"module_id","name","version","device","backend","capabilities":[...]}`（与 module.toml 声明一致） |
| `POST /predict/chat` | 见 §3 |
| `POST /predict/transcribe` | 见 §4 |
| `POST /predict/speak` | 见 §5 |

请求格式：multipart（`file` + `params` JSON 字段，平铺字段优先，comfyui-bridge 同款）
与 JSON body（`input`/`input_text`/`input_path` + `params` 对象）双支持（ADAPTER_API §2.3 A/B/C）。
成功响应：`{"status":"completed","output_type":...,"result":...,"output_path":...,"elapsed_seconds":...,"usage":{...}}`；
失败：4xx/5xx `{"error":"<message>"}`（错误映射见 §6）。

## 3. chat 能力

### 3.1 普通文本（原始保真，出口三原则 ③）

- 输入：`input`/`input_text`（JSON body）或文本文件（multipart）。
- `system_prompt` 参数 → system 消息；`context_in`（§3.3）注入 system 上下文。
- **响应全文原样作为主产物**（`output_type="text"`，不截断不改写）；
  `output_format` ≠ json 且有 `output_path`（引擎产物协议注入）时落盘为文件返回
  `output_type="file"`。
- `response_format` 参数原样透传上游（如 `json_object`）。

### 3.2 SRT 就地重组（格式就地保持，出口三原则 ②）

输入命中 SRT（`.srt` 文件，或文本含 `HH:MM:SS,mmm --> HH:MM:SS,mmm` 时间轴行）时：

1. **解析**：按空行分块；序号行/时间轴行**原文保留**；缺序号容忍；非法块 → 400（不静默错位）。
2. **保护剥离**：条目内行首 `{\...}` 位置标签与整条包裹标签（`<i>`/`<b>`/`<u>`/`<font …>`）
   代码级剥离，变换后原样回填（三原则 ③ 的代码级保证）；正文行内其余标签随正文直送（best-effort，见 §8 局限）。
3. **上游只见文本**：批量单次调用，协议行 `##N## <文本>`（条目内换行转义为字面量 `\n`）；
   系统提示词 = 任务指令（`system_prompt`，缺省为校对润色）+ `context_in` + 协议约束。
   **时间轴绝不外送**。
4. **回映射**：按 `##N##` 标记逐条回填；缺失序号**逐条补调**（上限 32 条）；
   仍不齐 → 502 明确报错（`refusing to misalign timeline`），绝不静默错位。
5. **渲染**：仅替换正文块；时间轴行/序号行/换行风格（CRLF/LF）/BOM/结尾换行逐字保真。

**验收断言（tests 覆盖）：SRT 入 → SRT 出，每条起止时间与序号逐条相等。**

### 3.3 context_in 端口语义（计划6补充：可选，前文/参考上下文 → 注入 system 上下文）

接受任一形态（可叠加）：
- JSON body `context_in` / `context`：字符串或字符串数组（数组以 `---` 分块拍平）；
- multipart 名为 `context_in` 的字段；
- multipart 文件部件文件名以 `context` 开头（现引擎多文件统一发 `file` 字段，按文件名甄别；
  主输入取首个非 context 部件）；
- `params.context_in`（params 注入形式，module.toml 已声明）。

## 4. transcribe 能力

- **纯音频**（multipart 文件 / `input_path`）：`POST {base}/audio/transcriptions`，
  multipart 携带 `file`/`model`/`language`（auto 不发送）/`prompt`/`temperature`/`response_format`。
  - `output_format="srt"`（引擎产物协议）且未显式给 `response_format` → 上游
    `response_format="srt"`，**输出上游返回的 SRT**（透传）；
  - 显式 `response_format="verbose_json"` + `output_format="srt"` → 由 segments 本地构建 SRT
    （`HH:MM:SS,mmm`，faster-whisper 同款）；上游无 segments → 502 明确报错；
  - `response_format` ∈ srt/vtt/text → 文本透传；
  - 缺省 json → `output_type="json"`，result = `{text, segments?, language?, duration?}`。
- **SRT 输入**（`.srt` 文件 / `input_path` 指向 .srt / 文本命中 SRT 形状）：
  走 §3.2 同一重组引擎，逐段文本变换经上游 `/chat/completions`，时间轴逐条对齐回填
  （用例：本地 ASR 产出的 SRT 云端修正/翻译）。
- 无输入 → 400。

## 5. speak 能力

- 输入：`input`/`input_text`/`input_path`/文本文件。
- `POST {base}/audio/speech` JSON body：`model`/`input`/`voice`（缺省 `alloy`）/
  `response_format`（缺省 `mp3`，决定产物扩展名）/`speed`。
- 响应字节落盘：`params.output_path`（引擎注入）优先，否则 `output/output.mp3`；
  返回 `output_type="file"` + 绝对路径。
- 上游不返回 token usage → usage 记录为 null（如实）。

## 6. 错误映射（对齐 comfyui-bridge §4.2）

| 现象 | HTTP | error 语义 |
|---|---|---|
| 缺 `model` / 缺输入 / params 非法 / 非法 SRT | 400 | missing parameter / missing input / invalid params / invalid SRT |
| 未知 capability | 404 | （FastAPI 路由） |
| 上游 4xx（含 401/429） | 502 | `openai rejected request: HTTP <code>: <脱敏正文>` |
| 上游 5xx | 502 | 同上 |
| 连接失败/传输异常 | 502 | `openai unreachable: <脱敏>` |
| 上游超时 | 504 | `openai request timeout` |
| 上游响应缺字段/非 JSON | 502 | `openai malformed response` |
| SRT 映射不齐（补调后仍缺） | 502 | `SRT realignment failed: ... refusing to misalign timeline` |

## 7. mock 上游行为矩阵（tests/mock_openai.py，纯标准库，测试永久不触外网）

| 端点 | 行为 |
|---|---|
| `POST /v1/chat/completions` | 按 `chat_mode` 变换末条 user 消息：`upper`（有 `##N##` 标记→逐行转大写保标记；无标记→整体大写）、`drop`（丢第 2 条 → 触发逐条补调）、`garbage`（批量→无标记文本、逐条→空，逼出错位拒绝路径）、`echo`（原样回显）；返回 OpenAI 形状 + usage（字符数确定性伪计数） |
| `POST /v1/audio/transcriptions` | multipart 解析；`response_format` ∈ srt/vtt → 固定 SRT/VTT 文本；`verbose_json` → text+segments+language+duration；json → text+usage |
| `POST /v1/audio/speech` | 校验 `input`/`voice` → `b"MOCK-AUDIO-BYTES:"+voice`；Content-Type 按格式 |
| `GET /v1/models` | 固定清单（/health 私网探测） |

错误注入开关（运行期可翻转）：`fail_chat`（恒 500）、`fail_auth`（恒 401 且错误正文
**回显收到的 Authorization 值**——供适配器密钥脱敏断言）、`require_auth`（缺头 401）。
请求记录：`chat_requests` / `transcribe_requests` / `speech_requests` / `auth_headers`。

## 8. 边界与 M2 留白（诚实声明）

- **密钥**：模块不存储/不落盘/不回显密钥。接受运行时注入：
  `params.api_key`（运行时值）> `params.key_env`（环境变量名引用，`key_ref` 形态）>
  env `OPENAI_API_KEY`。密钥集中管理（`[[llm.providers]]` profile、掩码、测试连接）
  属 **T-24**；引擎面 `ModelSource::api` / external 池 / 任务元数据记账属 **M2**。
- **usage 记账**：M1 收敛为模块侧产物（`usage.json` 侧产物 + 响应内 `usage` 字段）；
  写入任务元数据/external 池留给 M2（T-23 范围裁剪）。
- **usage.json 落点**：`EP_WORKSPACE`（任务工作目录）> `params.output_path` 同目录 >
  `MODULE_DIR/output/`。内容仅含端点路径/模型名/token 数，无密钥无完整 URL；
  写入失败仅告警不阻塞主产物。并发的同目录写入以最后一次为准（平台对未声明
  max_concurrent 的模块默认串行准入，无实际并发冲突）。
- **SRT 重组局限**：仅支持毫秒逗号 SRT（VTT/ASS 不重组）；跨条目延续的包裹标签、
  正文行内标签（如 `<i>a</i> b` 片段包裹）随正文直送上游（系统提示词要求保留，
  best-effort）；多个连续结尾空行归一为一个；条目内换行以字面量 `\n` 哨兵传输，
  正文含字面反斜杠序列的场景理论上可能被还原（真实字幕中极罕见）。
- **SRT 协议碰撞**：正文含字面 `##N##`（用户原文撞上协议标记，病态输入）时，
  该标记会被当作后续条目的起点截断前条正文并产生幻影条目（实测行为，审查 P2-1
  复核结论：内嵌标记必然终结前段、不会泄漏进已采纳正文，但碰撞正文会丢失）。
  真实字幕中极罕见；防御性转义留 M2。
- **n>1 多候选目录产物**（出口三原则 ③ 第三条）：M1 不实现（引擎无目录产物语义）。
- **`embed` / `generate_image`**：计划6补充列为可选能力，M1 不实现。
- **不碰引擎**：零 Rust 改动是 M1 定位；本模块仅经标准 `/predict/{capability}` 接入。
