# ComfyUI 桥接模块集成 — 完整执行计划

> 版本：v1.0（2026-08-29 多轮讨论定稿）
> 状态：待执行。本文件即可直接交给执行 AI（强并行、善开子代理）按 §4 编排规则落地。

---

## 一、背景与目标

EntryPoint 是 server 形态的 AI 模型编排平台（axum daemon + DAG 管线引擎 + React WebUI）。
本功能目标：**让管线能把产物自动发给本地/远程 ComfyUI 执行、取回结果继续流转**；首期只交付
ComfyUI，同时沉淀可复制的"桥接模块"模式，未来接入 A1111、n8n 等任意带 HTTP API 的工具。

技术基础：ComfyUI 自带完整 HTTP API（默认 `127.0.0.1:8188`）——`POST /prompt` 提交 API 格式
工作流、`POST /upload/image` 上传输入、`GET /history/{prompt_id}` 查结果、`GET /view` 下载产物、
`POST /interrupt` 中断、`GET /system_stats` 系统状态。平台侧无需感知工作流内部逻辑。

## 二、已确认决策（多轮讨论定稿，执行时不可偏离）

| # | 决策 | 内容 |
|---|---|---|
| D1 | 技术路线 | **桥接模块**：新增 `modules/comfyui-bridge/`，Python adapter 把 ComfyUI HTTP API 包装成平台标准模块。管线引擎（ep-core）、DAG 校验、模块自动拉起/健康检查/空闲回收全部零改动复用。引擎级 `service` 节点明确**不做**，仅记为二期备选（见 §9） |
| D2 | 进程管理 | **仅连接**已运行的 ComfyUI 实例（`COMFYUI_URL` 环境变量 / `base_url` 参数），平台不拉起、不托管 ComfyUI 进程 |
| D3 | 进度 | **粗粒度**：adapter 轮询期间打印 `EP-PROGRESS:NN%` 日志（前端已有解析约定），不改引擎 |
| D4 | 工作流供给 | **首期就做 WebUI 上传**：用户在模块详情页上传 ComfyUI 导出的 API 格式工作流 JSON，存于模块 `workflows/` 目录，管线节点按名字引用 |
| D5 | 注入机制 | **通用 inject 映射**（§3.3），支持多组文件/文本/常量注入与多输出 |
| D6 | 职责边界 | 平台只保证向 ComfyUI 发送**合法数据包**并取回产物；注入前校验映射键存在性，工作流内部逻辑不归平台管 |
| D7 | 首期场景 | 通用机制 + 2 个示例模板（图片放大、风格化/修复类，以 ComfyUI 内置节点为准）+ **txt2img** 文生图（提示词可由上游文本节点注入） |
| D8 | 验证 | **mock ComfyUI 服务器为主**（全端点仿真，自动化测试不依赖真机）；真机终验用本机整合包 `/home/bob/Desktop/AI_Applications/ComfyUI-aki/ComfyUI-aki-v3/`（可选步骤） |
| D9 | 文档 | **面向用户创作的详细文档是核心交付物**（§5），不是附属品 |

---

## 三、核心契约（F1 冻结，所有并行工作的唯一依据）

### 3.1 模块清单契约（`modules/comfyui-bridge/module.toml` 形状）

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
- **不声明 `[[models]]`**（manifest 中 models 可选；桥接模块无权重）。
- 依赖仅 `fastapi / uvicorn / httpx`，落盘前与 `config/constraints.txt` 核对兼容性。

### 3.2 Adapter HTTP 契约

**平台标准端点**（遵守 `docs/ADAPTER_API.md`）：

| 端点 | 行为 |
|---|---|
| `GET /health` | 代理探测 ComfyUI `GET /system_stats`，不通返回 503（使"管线执行前自动拉起"失败语义直白） |
| `GET /info` | 桥接版本 + ComfyUI 可达性/版本 + `GET /queue` 摘要 |
| `POST /predict/generate` | 同步阻塞全流程：收上游产物（multipart，按上游节点 id 索引）→ 解析 inject → 文件类输入先 `POST /upload/image` → 提交 `POST /prompt` → 轮询 `GET /history/{prompt_id}`（期间打印 `EP-PROGRESS:NN%`）→ `GET /view` 下载产物到注入的 `output_path` 目录 → 返回 `{"status":"completed","output_type":"file","result":"<主产物绝对路径>"}` |

客户端断开时尽力调 `POST /interrupt`（best-effort，文档诚实声明，对齐 §4.3 语义）。

**工作流管理端点**（供 WebUI 经 daemon 代理调用）：

| 端点 | 行为 |
|---|---|
| `GET /workflows` | `[{name, size_bytes, mtime}]` |
| `POST /workflows` | multipart `file` 字段；校验为合法 JSON 且为 API 格式（顶层对象、每个值含 `class_type` 与 `inputs`）；落盘 `workflows/<清洗后文件名>.json`，重名覆盖并返回 `{name, replaced}` |
| `DELETE /workflows/{name}` | 删除 |

### 3.3 inject 映射语法（D5 细则）

`inject` 为 JSON 对象，键 = `<工作流节点id>.<inputs字段名>`，值 = 来源表达式：

| 表达式 | 语义 |
|---|---|
| `$input` | 首个上游产物文件（对齐平台既有 `{input}` 约定） |
| `$input.<上游节点id>` | 定向引用指定上游节点的文件产物（多条输入边时必需） |
| `$input.<上游节点id>`（上游为文本产物） | 文本注入字符串字段（txt2img 提示词场景） |
| 字面量 | 数字/字符串/布尔常量（seed、steps 等） |

执行规则：
1. 文件类来源先 `POST /upload/image` 上传、再把返回文件名写入字段；文本/字面量原样写入。
2. **注入前逐项校验键存在**；缺失立即报错并列出模板可用节点清单（D6），不提交给 ComfyUI。
3. 未映射字段保留模板默认值；键天然唯一，无冲突。
4. `output_nodes` 指定取回哪些输出；全部下载到产物目录，第一个为主产物返回下游。

多组注入示例：

```json
{
  "3.inputs.image": "$input",
  "5.inputs.image": "$input.ref",
  "7.inputs.text":  "$input.prompt",
  "9.inputs.seed":  42,
  "9.inputs.steps": 28
}
```

### 3.4 ComfyUI REST 端点清单（外部事实，客户端封装范围）

`GET /system_stats` · `POST /upload/image` · `POST /prompt` · `GET /history/{prompt_id}` ·
`GET /view?filename=&type=output` · `POST /interrupt` · `GET /queue`

### 3.5 daemon 模块代理端点（唯一的 Rust 新增）

新增 `crates/ep-daemon/src/api/module_proxy.rs`：

- `GET|POST|DELETE /api/modules/{module_id}/extra/{*path}`：查模块端口注册表取运行中适配器端口 →
  原样转发（method/headers/body，multipart 透传）→ 回传响应；模块未运行返回 409；转发目标仅限
  127.0.0.1 适配器（同一信任域）。
- `api/mod.rs` 增一行路由注册。
- 前端调用形如 `/api/modules/comfyui-bridge/extra/workflows`。

### 3.6 前端契约（WebUI 工作流上传）

- `frontend/src/api/client.ts` 增 `moduleExtra(moduleId, path, init)` 封装。
- 模块详情页新增"工作流管理"卡片（对 `genre="comfyui"` 类模块显示）：列表 / 上传（接受 .json）/ 删除。
- i18n 键入 `i18n/locales/{en,zh-CN}/modules.json`，新前缀 `comfyui.*`。
- **管线编辑器零改动**：`workflow` 参数走 capability params 自动表单（string 类型）。

---

## 四、文件级任务分解与多子代理并行编排规则

### 4.1 全部新增文件（可整体删除回滚）

```
modules/comfyui-bridge/
├── module.toml                 # §3.1 契约
├── requirements.txt            # fastapi uvicorn httpx
├── adapter.py                  # §3.2 全部端点 + inject 引擎
├── comfy_client.py             # §3.4 端点封装（超时/退避轮询/错误分类/本机 no_proxy、远程尊重代理）
├── workflows/                  # 用户上传与示例模板落盘处
│   ├── upscale_4x.api.json     # 示例 1（仅用 ComfyUI 内置节点）
│   ├── style_transfer.api.json # 示例 2（可按 aki 整合包实机可用性调整选题）
│   └── txt2img.api.json        # 示例 3（提示词注入点标注）
├── tests/
│   ├── mock_comfyui.py         # 全端点仿真服务器（并行开发与离线测试的枢纽）
│   ├── conftest.py
│   ├── test_comfy_client.py
│   ├── test_inject.py          # 多组注入/多上游/非法键报错/多输出
│   └── test_adapter_flow.py    # 对 mock 跑完整 generate
├── CONTRACT.md                 # 第 0 轮冻结契约（§3 全部 + 客户端签名表 + mock 行为矩阵 + 错误映射表）
└── README.md                   # §5 核心交付物

config/pipelines/
├── comfyui_demo.toml           # file_input → comfyui-bridge.generate → file_output（节点 timeout_secs=1800）
└── comfyui_txt2img_demo.toml   # 文本上游 → generate（验证 $input.<id> 文本注入）
```

### 4.2 存量文件修改（仅 3 处小改动）

| 位置 | 改动 |
|---|---|
| `crates/ep-daemon/src/api/module_proxy.rs` | 新文件；`api/mod.rs` 一行路由 |
| `crates/ep-webui/frontend/src/api/client.ts` | `moduleExtra` 封装 |
| 模块详情页组件 + `i18n/locales/{en,zh-CN}/modules.json` | 工作流管理卡片 + 双语文案 |

### 4.3 轮次调度（执行 AI 编排蓝图）

**第 0 轮（串行，1 个契约主人）**
产出并冻结 `CONTRACT.md`：§3 全部内容 + `comfy_client.py` 方法签名表 + mock 端点行为矩阵 +
inject 语法表 + 错误映射表（ComfyUI 错误 → adapter 错误信息）。冻结后任何变更须回编排者仲裁并广播。

**第 1 轮（8 路全开，最大并行）**

| 代号 | 职责 | 独占文件域 |
|---|---|---|
| A | `comfy_client.py` + 单测（面向契约签名编程，mock 就绪前先写逻辑骨架） | `comfy_client.py`、`tests/test_comfy_client.py` |
| B | `adapter.py`（标准三端点 + 工作流管理端点 + inject 引擎）；对 A 用**契约签名+桩**编程，不等 A | `adapter.py`、`tests/test_inject.py`、`tests/test_adapter_flow.py` |
| C | `mock_comfyui.py` 仿真服务器（第 N 次轮询返回完成、`/view` 返回固定字节、错误注入开关）+ pytest 脚手架 | `tests/mock_comfyui.py`、`tests/conftest.py` |
| D | `module.toml` + `requirements.txt` + 3 份示例工作流模板（按 ComfyUI 内置节点 API 格式构造；第一步先核对 `config/constraints.txt`） | `module.toml`、`requirements.txt`、`workflows/*` |
| E | daemon `module_proxy.rs` + `Router::oneshot` 测试（进程内 mock adapter：运行中转发、未运行 409、multipart 透传）+ `api/mod.rs` 一行 | `crates/ep-daemon/src/api/module_proxy.rs`、`api/mod.rs` 路由行 |
| F | 前端 `moduleExtra` + 工作流管理卡片 + i18n（en/zh-CN 同步，键前缀 `comfyui.*`） | `api/client.ts`、模块详情页组件、`modules.json` 两语言 |
| G | 文档草稿（§5 全部章节骨架 + 正文 80%，实测命令留占位待回填） | `README.md`、`docs/ADAPTER_API.md` §6 增补、`docs/MODULE_SPEC.md` 增补 |
| H | 示例管线 2 份（TOML，依契约参数名编写，节点 `timeout_secs=1800`） | `config/pipelines/comfyui_*.toml` |

**第 2 轮（合流联调）**：B 接入 A 真实客户端 → 对 C 的 mock 跑通 `generate` 全流程（门禁 V3）；
E、F 各自完成独立验收。

**第 3 轮（收口）**：集成验证（V4/V5）→ G 回填实测内容定稿 → 合并顺序固定
**C → A → B → D → E → F → H → G**，每步合并后跑对应回归。

### 4.4 纪律条款

1. **文件所有权互斥**：上表文件域唯一归属；跨域需要改动时只能向编排者提契约变更请求，禁止直接改对方文件。例外：`api/mod.rs` 路由行由 E 独占。
2. **禁止触碰清单**：`crates/ep-core/` 全部、`executor.rs` / `dag.rs` / `runner.rs`、`pipeline-node.tsx` 内置节点定义、`config/constraints.txt`。发现确需改动时默认结论是"不改，模块侧解决"，升级编排者仲裁。
3. **契约冻结点**：
   - F1 = 第 0 轮末（全契约强冻结）；
   - F2 = 第 1 轮末（客户端签名、inject 语义、REST 形状——软冻结，只许加字段禁改语义）；
   - F3 = 文档定稿前（module.toml 参数键名冻结）。
4. **独占资源**：真机 ComfyUI（aki 整合包）与 8188/9800 端口、`cargo test --workspace` 全量回归为独占资源，同一时刻仅一个代理持有。
5. **每代理自带验收**：各自测试全绿即完成，不跨代理等待。
6. **回滚单元**：每道门禁失败时回滚粒度 = 单文件/目录；最差情况整体删除 `modules/comfyui-bridge/` + 还原 3 处存量改动即恢复原状。

### 4.5 集成门禁（逐级放行）

| 门禁 | 内容 | 放行条件 |
|---|---|---|
| V1 | mock 服务器自测（curl 全端点）+ `comfy_client.py` 对 mock 单测 | 全绿 |
| V2 | inject 引擎单测（多组注入/多上游/非法键报错/多输出） | 全绿 |
| V3 | adapter 全流程对 mock 跑通（等价"假 ComfyUI 真协议"） | `generate` 端到端产物落盘 |
| V4 | `cargo test --workspace` + `cargo clippy` 零回归；前端 `npm run build` 通过 | 零失败 |
| V5（可选真机终验） | 启动 aki 整合包 → `GET /api/modules` 见模块 → WebUI 上传工作流 → 跑 `comfyui_demo.toml` 断言产物 → 工作流管理卡片走查 → 任务卡片见百分比进度 | 全清单通过；无真机时以 V4 为验收上限并如实记录 |

---

## 五、用户创作文档要求（D9，核心交付）

`modules/comfyui-bridge/README.md` 必须包含：

1. **快速开始**：启动 ComfyUI（含 aki 整合包路径示例）→ 模块自动发现 → 第一条管线跑通，5 分钟内完成。
2. **工作流制作指南**：ComfyUI 开启开发者模式 → "Save (API Format)" 导出 → WebUI 上传步骤 → 节点 id / 字段名的查看方法（API 格式 JSON 结构解读）。
3. **inject 语法完整参考**：四类来源表达式逐条示例；多组注入示例（双图输入 + 提示词 + 常量）；txt2img 完整示例；`output_nodes` 多输出说明；常见错误排查（键不存在报错样例）。
4. **3 个示例模板逐一走读**：每个模板的节点图说明、注入点标注、配套管线 TOML。
5. **远程实例接入**：`COMFYUI_URL` / `base_url` 指向远程 + `--listen 0.0.0.0` 说明 + 安全提醒（ComfyUI 本身无认证，公网暴露须自行加鉴权/内网隔离）。
6. **限制与边界**：取消为尽力而为、长任务需设节点 `timeout_secs`、平台不保证工作流内部正确性（D6）。

配套文档增补：
- `docs/ADAPTER_API.md` §6 增"策略 C：桥接外部 HTTP 服务（ComfyUI 实例）"。
- `docs/MODULE_SPEC.md` 末尾增"桥接模块模式"小节——未来接入 A1111/n8n 的复制模板（目录结构、契约、测试模式）。

---

## 六、测试计划

1. `pytest modules/comfyui-bridge/tests/`（对 mock，CI 永久不依赖真机）：
   - 客户端：重试/超时/错误分类/退避轮询；
   - inject：全路径（多组、多上游、文本注入、非法键报错、多输出）；
   - adapter：全流程 + 工作流管理端点（非法 JSON 拒绝、路径穿越文件名清洗）；
   - `module.toml` 能被模块发现流程解析。
2. `cargo test -p ep-daemon`：module_proxy 路由 oneshot 测试（运行中转发、未运行 409、multipart 透传）。
3. `cargo test --workspace` 全量回归（引擎零改动承诺的最终证据）。
4. 前端：`npm run build` + 浏览器走查（模块发现 → 工作流上传/列表/删除 → 管线编辑器见模块节点 → 提交任务见进度）。
5. 真机：§4.5 V5 清单（可选）。

## 七、风险与缓解

| 风险 | 缓解 |
|---|---|
| ComfyUI API 格式工作流构造门槛高，示例模板与 aki 整合包节点集不符 | 模板仅用内置节点；V5 真机环节修正；文档教用户自己导出（主路径） |
| 长任务被节点默认 300s 超时杀掉 | 示例管线显式 `timeout_secs = 1800`；README 醒目标注 |
| 长任务期间模块空闲回收误杀 | 执行器调用即活跃触点（现有机制）；验收清单加"长任务不被回收"抽查 |
| `EP-PROGRESS` 轮询进度粒度粗 | 按已完成节点数/总节点数估算；接受粗粒度（D3） |
| 模块代理端点暴露面（0.0.0.0:9800） | 仅转发至 127.0.0.1 适配器，与既有模块端口同等信任域；文档标注不暴露公网 |
| 依赖与 `config/constraints.txt` 冲突 | D 代理开工第一步核对；fastapi/uvicorn/httpx 为现役模块同款，风险低 |
| 破坏既有行为 | 引擎零改动 + 存量仅 3 处小改；V4 全量回归兜底；回滚 = 删目录 + 还原 3 处 |

## 八、已否决的替代方案

1. **引擎级 `service` 节点 + ExternalBackend trait**：获得实时进度/队列感知/多后端扩展，但跨 3 交付物 10+ 文件、触碰冻结契约，首期价值不匹配成本。**记为二期触发条件**（§9）。
2. **固定约定注入（LoadImage 自动写）**：换自定义工作流即失效，被通用 inject 映射取代。
3. **平台托管拉起 ComfyUI 进程**：用户明确选择"仅连接"；整合包启动方式千差万别，不宜代管。
4. **TOML 内联工作流 JSON**：大文件难维护、无管理界面，被"上传 + 名字引用"取代。

## 九、二期备选（明确门控，不提前开工）

当"桥接模块 ≥3 个且出现共性瓶颈"（进度精度不足、并发排队需求、远程认证需求）时，重启评估引擎级方案：
新增 `builtin="service"` 节点 + 可插拔后端驱动（comfyui → a1111 → generic_http）、`ProgressMessage`
扩 `percent` 字段、服务级并发闸门与服务管理页。届时本计划 `comfy_client.py` 的语义即后端驱动的需求蓝本。

## 十、假设

- ComfyUI 实例由用户自行启动（本地 8188 或远程），平台启动时可不可达——`/health` 503 使失败语义清晰。
- 首期不做工作流可视化预览/编辑，上传即黑盒（D6）。
- txt2img 示例依赖 ComfyUI 侧已装基础模型（SD1.5/SDXL 任一），README 注明。
