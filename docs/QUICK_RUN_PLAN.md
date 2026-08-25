# 快速调用页（Quick Run）— 设计方案与多子代理并行执行计划

> 版本：v2（已确认） | 日期：2026-08-25
> 依据：3 路并行代码侦察报告（ep-daemon 执行/任务系统、模块生命周期看门狗、WebUI 前端），
> 所有 `文件:行号` 锚点基于 HEAD `4575fcce`。
> 状态：**决策点 §11 QD1–QD5 已按推荐项确认，执行中**（W0 契约冻结已完成）。
> 执行者为多子代理并行开发模式，规程见 §6。

---

## 1. 背景与目标

用户诉求归纳：

1. 很多场景不需要完整管线——"我只是想单独对一个音频文件做 ASR，让我开管线很难受"；
2. 需要一个**统一页面**，快速调用不同功能模块做 ad hoc 处理，**与管线同级**；
3. 任务发出后仍通过任务中心追踪；
4. 业务完成后一段时间模型无更多业务就**自动下线**节约性能。

### 1.1 目标

| # | 目标 |
|---|---|
| G1 | 新增顶级导航「快速调用」（路由 `/run`），以**能力目录**聚合全部已装模块的能力：选能力 → 给输入 → 调参 → 提交，主流程不出一页 |
| G2 | 每次调用即一等公民任务：任务中心可追踪进度、取消、下载产物 |
| G3 | 输入形态全覆盖：文件型（音频/视频/图片）与**文本型**（TTS 等 text→* 能力）均可直跑 |
| G4 | 冷启动不阻塞提交：提交立即受理返回，模块拉起过程在任务中可见 |
| G5 | 模型空闲自动下线闭环补全：配置文档缺口修补 + UI 可见性提示 |
| G6 | 模块页现有「直跑抽屉」保留且行为不变（抽取为共用组件） |

### 1.2 排除（列入 §9 Phase 2 backlog）

- 多文件批量提交（客户端循环提交即可实现，v1 先单文件）
- 空闲倒计时实时徽章（需后端暴露 `last_active`，见 §9）
- 参数预设 / 调用历史的持久化
- `/v1` 外部推理 API 的任何变更（自动化集成方不受影响）

---

## 2. 现状盘点（侦察结论 + 锚点速查）

**结论先行：后端"直跑即任务"内核与空闲回收看门狗均已存在，本特性约 80% 工作量在 WebUI 层，后端仅需两个向后兼容的小增强。**

### 2.1 直跑任务化内核（已有，勿重建）

| 设施 | 锚点 | 说明 |
|---|---|---|
| `POST /api/execute/single` | `crates/ep-daemon/src/api/execute.rs:377-529` | 直跑端点；202 返回 `{"task_id"}`，**不是**同步阻塞拿结果 |
| `execution::submit_direct_full` | `crates/ep-daemon/src/execution.rs:1587` | 把单模块调用编译成退化三节点 DAG 走完整管线任务链路 |
| `build_direct_pipeline` | `execution.rs:1648-1736` | `input(file_input) → run(module) → output(file_output)`；Json/Text 输出能力省略 output 节点（两节点）；`pipeline.id = "direct/{module_id}"` |
| 任务注册表 | `ep-core/src/task_registry.rs` + `runtime/tasks/*.json` | 排队/运行/终态全持久化，重启回读 |
| WS 进度 | `/ws` 与 `/ws/progress`，`state.rs:139 progress_tx` | 节点级实时推送 |
| 产物归集 | `finalize_task` `execution.rs:1107-1283` | 硬链接至 `files/{node_id}/`，`GET /api/tasks/{id}/artifacts[/node_id]` 下载 |
| 取消 | `POST /api/tasks/{id}/cancel` → `request_cancel` | 协作取消传播到引擎 |
| 自动拉起 | `api/autostart.rs:103 ensure_module_running` | 模型预检→设备→venv→端口→启进程→轮询健康；handler 在提交前同步调用（`execute.rs:512`），排队准入后另有 `ensure_pipeline_modules` 幂等兜底（`execution.rs:891`） |
| 参数校验 | `validate_and_fill_params` `execute.rs:296-321` | 必填/类型/枚举 + 默认值注入 |
| 文本输入先例 | `api/inference.rs:654-686` + `upload.rs:1040 store_input_file` | v1 门面把 JSON 文本输入物化为服务器文件后走同一直跑链路 |

### 2.2 模型空闲自动下线（已有，G5 仅剩补全）

| 要素 | 事实 |
|---|---|
| 配置 | `[modules].idle_timeout_secs`，缺省 **1800s**，`0`=停用；定义 `ep-core/src/config.rs:387-405`；设置页已有该字段（`settings.tsx:979-981`）；前端类型已有（`types.ts:131-132`） |
| 巡检 | daemon 内 tokio 任务，**30 秒一轮**（`main.rs:364-429`）；实时读配置，运行期改即时生效 |
| 判定 | `now - last_active >= timeout` 即回收；`last_active<=0` 保守跳过；排队/运行中任务引用的模块 busy 守卫豁免 |
| 活跃触点（bump） | 手动启动、任务提交、节点开始、节点完成、任务终态（共五处，见 `modules.rs:298`、`execution.rs:663/1794/1821/1233`） |
| 覆盖面 | **手动启动与自动拉起一律受管**（`ServiceInstance` 无来源标志，设计意图如此） |
| 回收动作 | 杀进程树 + 释放端口；**venv 与模型权重保留**，下次按需秒级重载 |
| **缺口** | `docs/CONFIG_REFERENCE.md` 没有 `[modules]` 章节（文档滞后） |

### 2.3 前端直跑抽屉（已有，将抽取复用）

| 设施 | 锚点 |
|---|---|
| 抽屉主体 | `pages/modules.tsx:329-950`：能力下拉 ← `ModuleResponse.capabilities`；参数表单 `ParamField`（L332，schema 数据驱动）；产物预览 `fetchArtifactPreview`（L411-457）；输入区 = 服务器路径直填 / 浏览器上传回填（`api.uploadInput` → `POST /api/upload/input`） |
| 状态机 Hook | `hooks/use-direct-exec.ts`：提交（300s AbortController 超时，冷启动等待）→ 202 task_id → 1.5s 轮询 + WS 过滤 → 终态拉产物；代数防串染 |
| 类型 | `types.ts:484-503` `DirectExecRequest/DirectExecResponse/UploadInputResponse` |

### 2.4 其他相关事实

- `GET /api/modules` 已透传 manifest `capabilities`（P0-1 已修），前端可客户端聚合能力目录，**无需新目录端点**；
- 任务列表当前把 `pipeline_name`（直跑任务即 `direct/<module_id>` 原文）直接展示（`tasks.tsx:330/445`），未美化；
- 导航数据源唯一：`components/layout/sidebar.tsx:21-27 NAV_ITEMS`（桌面侧栏与移动抽屉共用）；
- 路由注册：`App.tsx:51-60`；
- i18n：仓库根 `i18n/locales/{zh-CN,en}/*.json` 扁平键，命名空间注册于 `frontend/src/i18n/index.ts:36-59`；
- 门禁命令：Rust `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`；前端 `npm run build`（=`tsc -b && vite build`）/ `npm run lint`（oxlint）；前端变更后须把 `crates/ep-webui/static` 构建产物随仓库提交；
- 300s 请求总超时中间件对 upload 路径豁免（`main.rs:588 is_timeout_exempt_path`），大文件上传无忧。

---

## 3. 差距分析 → 目标映射

| 目标 | 差距 | 改动 |
|---|---|---|
| G1 统一页 | 直跑埋在模块卡抽屉里，无能力目录视角 | 新页面 `/run` + 导航项（纯前端为主） |
| G2 任务追踪 | 已可用 | 仅美化：任务页识别 `direct/*`、类型筛选、深链高亮 |
| G3 文本输入 | `DirectExecRequest` 必填 `input_path`，TTS 类能力无法直跑 | 后端加可选 `input_text`（服务端物化，§4 D2） |
| G4 冷启动不阻塞 | handler 提交前同步等健康（最长 30s~数分钟） | 后端加可选 `lazy_start`（§4 D3） |
| G5 自动下线 | 机制已在；缺 CONFIG_REFERENCE `[modules]` 章节 + UI 提示 | 文档 + `/run` 页与模块卡静态提示（§4 D5） |
| G6 抽屉保留 | 组件内联在 modules.tsx | 机械抽取共用组件（§4 D6） |

---

## 4. 设计

### D1 页面与信息架构（`/run` 快速调用）

- **导航位置**：`sidebar.nav` 序列 `仪表盘 / 模块与模型 / 管线编辑器 / 快速调用 / 任务中心 / 设置`（图标 Zap）。
- **布局**（宽屏左右分栏，窄屏上下堆叠）：
  - **左栏 · 能力目录**：数据源 = `GET /api/modules`（capabilities）× `GET /api/models`（模型就绪状态 join）客户端聚合；顶部 category chips 筛选（asr/tts/denoise/ocr/image/video/translate/custom，来自 manifest `category`）；条目 = 模块名 + 能力名 + `input_type→output_type` + 状态点（运行中/已停止/模型未就绪）。
  - **右侧工作台**：选中能力详情（描述、大小限制、变体）→ 输入区（按 `input_type` 切换：文件 = 路径直填 + 上传按钮，同抽屉；text = textarea）→ 参数表单（复用抽取后的 `ParamField`）→ 执行按钮。
  - **会话任务区**：本次会话内提交的任务卡片列表（内存态）：task_id、状态徽章、节点进度（WS）、产物预览/下载、「在任务中心查看 →」深链 `/tasks?focus=<task_id>`。
- **空态/降级**：模型未就绪 → CTA 跳转模块页下载；模块未装能力 → 目录不展示；`idle_hint` 静态提示"模型空闲 N 分钟后将自动下线（设置 → 模块生命周期可调，0 = 常驻）"，N 读 `config.modules.idle_timeout_secs`。

### D2 文本输入：`input_text`（后端，向后兼容）

`POST /api/execute/single` 新增**可选**字段 `input_text: string`：

- handler 物化为 `workspace/uploads/<module_id>-<capability>-<ts>.txt`（复用 `store_input_file`，与 v1 门面 `inference.rs:654-686` 同款先例），后续走既有 `file_input` 路径，`build_direct_pipeline` 零改动；
- `input_path` / `input_text` 二选一，均缺失 → 400；
- 理由：保持 ADAPTER_API/AUTOMATION 语义一致（一切输入皆文件），前端无需 Blob 包装黑魔法。

### D3 提交时机：`lazy_start`（后端，向后兼容）

新增**可选**布尔 `lazy_start`：

- `false`（缺省）：现状不变——handler 同步 `ensure_module_running` 等健康后再建任务（抽屉/v1 行为分毫不动）；
- `true`：跳过提交前预启动，校验（manifest/capability/params/输入）照旧同步快失败，然后立即建任务入队 202；模块拉起由准入后 `ensure_pipeline_modules`（`execution.rs:891`，幂等安全网）完成，**拉起失败计入任务错误、任务 failed**——冷启动全程任务可见可追踪，正是 G4 体验；
- `/run` 页固定传 `lazy_start: true`。

### D4 任务中心的直跑可视化（前端）

- `pipeline_id` 前缀 `direct/` 为天然判别符：展示为徽章「直跑」+ 模块名（用已加载的 modules 列表 join，找不到回退原文）；
- 列表加类型筛选 chips：全部 / 管线 / 直跑（客户端过滤，不加查询参数）；
- 支持 `?focus=<task_id>`：进入时展开并滚动高亮该任务（一次性消费 searchParams）。

### D5 空闲自动下线补全

1. `docs/CONFIG_REFERENCE.md` 补 `[modules]` 章节（字段、缺省、0 语义、触点说明）；
2. `/run` 页 `idle_hint` 静态提示（D1）；模块页卡片描述行追加同等提示（随 FE-A 顺带）；
3. 实时倒计时徽章 → §9 backlog。

### D6 直跑组件抽取（前端，行为不变）

从 `modules.tsx` 机械搬移到 `components/quick-run/`（零逻辑变更）：

- `param-field.tsx` ← L332-409 `ParamField`
- `artifact-preview.tsx` ← L411-457 类型 + 常量 + `fetchArtifactPreview`

抽屉改为 import；`use-direct-exec.ts` 原样共用。

---

## 5. 契约冻结（W0 基线内容，并行期不得偏离）

### 5.1 HTTP（仅增可选字段，其余不变）

```
POST /api/execute/single
{ module_id, capability, params?, input_path?,
  input_text?: string,     // 新增；与 input_path 二选一
  lazy_start?: boolean }   // 新增；缺省 false
→ 202 {"task_id"}          // 错误映射不变：400 校验 / 409 ModelNotReady / 429 QueueFull
```

### 5.2 TS 类型（W0 由集成代理预先落盘，各代理只读）

```ts
export interface DirectExecRequest {
  module_id: string
  capability: string
  params?: Record<string, unknown>
  /** 服务器本地输入文件路径（与 input_text 二选一） */
  input_path?: string
  /** 文本型输入：服务端物化为 workspace/uploads 下 .txt 后走文件链路 */
  input_text?: string
  /** true = 跳过提交前同步等健康，模块拉起在任务内完成 */
  lazy_start?: boolean
}
```

> 注意：`input_path` 由必填改为可选——后端 serde 同步 `Option<String>` + 双字段二选一校验。

### 5.3 组件接口（FE-A 实现，FE-C 按此盲写 import，W3 tsc 兜底）

```ts
// components/quick-run/param-field.tsx
export function ParamField(props: {
  name: string
  schema: CapabilityParamSchema
  value: unknown
  onChange: (value: unknown) => void
}): JSX.Element

// components/quick-run/artifact-preview.tsx
export interface ArtifactPreview {
  nodeId: string; name: string
  kind: 'text' | 'image' | 'binary'
  text?: string; objectUrl?: string; size: number
}
export const TEXT_PREVIEW_EXTS: RegExp
export const IMAGE_PREVIEW_EXTS: RegExp
export function fetchArtifactPreview(
  url: string, nodeId: string, name: string,
): Promise<ArtifactPreview>
```

### 5.4 i18n（W0 一次性全量落盘，杜绝并行撞键）

- 新命名空间 `run`：新文件 `i18n/locales/{zh-CN,en}/run.json`，注册进 `src/i18n/index.ts`；
- `components.json` ×2 追加 `sidebar.nav.quickrun`（"快速调用" / "Quick Run"）;
- `run.json` 键清单（≥28 键，两语言同集）：`title / description / categoryAll / selectCapability / noCapabilities / input.file / input.text / input.pathPlaceholder / input.textPlaceholder / input.upload / input.uploading / input.hint / params / submit / startingHint / accepted / openInTasks / nodeInput / nodeRun / nodeOutput / artifacts / download / previewFail / idleHint / modelNotReady / goModules / statusRunning / statusStopped`；
- `tasks.json` 归 FE-B 增量（`type.all / type.pipeline / type.direct / directBadge` 等）。

### 5.5 路由与导航 diff

```tsx
// App.tsx：pipeline 之后插入
<Route path="/run" element={<RunPage />} />
// sidebar.tsx NAV_ITEMS：pipeline 之后插入
{ to: '/run', labelKey: 'sidebar.nav.quickrun', icon: Zap }
```

---

## 6. 多子代理并行开发规程

> 执行环境为大模型 + 子代理调度。总原则：**契约先行、文件所有权互斥、波次推进、每波门禁、集成代理终裁**。

### 6.1 所有权矩阵（互斥写权限，越界即违规）

| 代理 | 独占写权限 |
|---|---|
| **INT**（集成） | `api/types.ts`、本计划文档、PROGRESS.md、最终合并与 static 产物提交 |
| **BE** | `crates/ep-daemon/src/api/execute.rs` 及其新增测试文件、`docs/CONFIG_REFERENCE.md` |
| **FE-A**（抽取重构） | `pages/modules.tsx`、`components/quick-run/{param-field,artifact-preview}.tsx`、`i18n/locales/*/models.json` |
| **FE-B**（任务页） | `pages/tasks.tsx`、`i18n/locales/*/tasks.json` |
| **FE-C**（/run 页） | `pages/run.tsx`(新)、`components/quick-run/{capability-catalog,run-workbench}.tsx`(新)、`App.tsx`、`layout/sidebar.tsx` |
| **DOC** | `README.md`、`DESIGN.md`、`docs/WEBUI_GUIDE.md` |

规则：
1. 只写自己列内的文件；发现必须改他人文件的诉求 → 写入报告「越权需求」节，由 INT 处理；
2. `src/i18n/index.ts`、`client.ts`、`types.ts`、`run.json`、`components.json` 全部在 W0 由 INT 落盘完毕，W1 起无人再碰（FE-B 对 tasks.json 的增量是唯一例外）；
3. FE-A 与 FE-C 在 `components/quick-run/` 下建**不同文件名**，互不冲突；FE-C 对 `param-field` 的 import 按 §5.3 契约盲写，W3 tsc 兜底。

### 6.2 波次编排

```
W0（INT 串行）：契约落盘 → 基线 commit（前后端门禁全绿）
W1（4 代理并行）：BE ∥ FE-A ∥ FE-B ∥ DOC → 逐个回报，INT 依序审查合并
W2（FE-C，可内部再拆 2 子代理并行：capability-catalog ∥ run-workbench，均为纯新增文件）
W3（INT 串行）：集成门禁全家桶 + static 产物提交 + E2E 冒烟 + PROGRESS.md 记录
```

- W1 各代理彼此零依赖（FE-C 后置正是为了消费 FE-A 的真实导出，消除盲写面）；
- W2 的 FE-C 可自行再拆子代理，但拆出者同样受 §6.1 所有权约束（文件名级互斥）。

### 6.3 子代理 prompt 规程（每个实现代理的 prompt 必含六段）

```text
【角色】W1-BE 实现代理（只实现，不做计划外重构）
【目标】一句话（对应本计划某 G 条目）
【独占文件】<§6.1 该行列表>（禁触其他文件；越权诉求写入报告）
【冻结契约】<粘贴 §5 相关节原文>
【现状锚点】<§2 相关 文件:行号 清单>
【完成定义】
  - 验证命令及预期（见 §6.4）
  - 报告格式：改动文件+±行数 / 测试输出尾部 20 行 / 偏离契约声明 / 越权需求
```

探索类子代理一律只读；实现类子代理禁止改共享文件。

### 6.4 门禁（每代理自查 + INT 合并后复跑）

| 层 | 命令 |
|---|---|
| Rust | `cargo fmt --check` ; `cargo clippy --workspace --all-targets -- -D warnings` ; `cargo test --workspace` |
| 前端 | `cd crates/ep-webui/frontend && npm run build && npm run lint` |
| i18n | zh-CN 与 en 的键集合一致（W3 INT 用脚本比对 `Object.keys`） |
| 产物 | W3：`npm run build` 后将 `crates/ep-webui/static` 随仓库提交（README 约定） |

### 6.5 提交纪律

- 每代理一个 commit，沿用仓库风格：`feat(quickrun)/w1-be: ...`、`refactor(quickrun)/w1-fea: extract direct-run components`；
- W1 合并顺序 = BE → FE-A → FE-B → DOC，每次合并后 INT 复跑该侧门禁，红了当场修（修动计入 INT 的 fixup commit）；
- 禁止 force-push / amend 他人提交。

---

## 7. Wave 分解与验收标准

| Wave | 代理 | 内容 | 验收 |
|---|---|---|---|
| W0 | INT | §5 契约全部落盘（types.ts、i18n 全量键、计划文档定稿） | 双侧门禁全绿；`tsc` 通过（run.json 已注册） |
| W1-BE | BE | D2 `input_text` 物化、D3 `lazy_start`、CONFIG_REFERENCE `[modules]` 章节；新增 ≥3 单测（input_text 物化 / 双输入缺失 400 / lazy_start 跳过预启动） | cargo 三连绿；现有 execute_single 测试零改动通过（向后兼容证明） |
| W1-FEA | FE-A | D6 抽取 + 抽屉改 import + 模块卡 idle 提示一行 | build/lint 绿；抽屉手工回归：能力→上传→提交→预览全流程不变 |
| W1-FEB | FE-B | D4 徽章/筛选/focus 深链 | build/lint 绿；`direct/*` 显示「直跑 · 模块名」，筛选生效 |
| W1-DOC | DOC | WEBUI_GUIDE 新章节、DESIGN.md §5 页面清单、README 特性行 | 文档交叉链接有效 |
| W2 | FE-C(+2子) | D1 完整页面：目录聚合/join 模型状态、分类筛选、双输入形态、复用 ParamField、useDirectExec、会话任务区、深链、idle_hint | build/lint 绿；冒烟清单 §8 第 1/2/6 条通过 |
| W3 | INT | 集成、i18n parity、E2E 全清、static 提交、PROGRESS.md Wave 记录 | §8 八条全过；双侧门禁全绿 |

---

## 8. E2E 冒烟清单（W3 验收）

1. **冷启动 ASR**：`/run` 选 faster-whisper/transcribe，上传 wav（lazy_start）→ 秒级 202 → 任务页 queued→running→completed → 下载 srt/txt；
2. **文本 TTS**：qwen3-tts textarea 提交 → 音频产物可下载；
3. **自动下线**：临时 `[modules].idle_timeout_secs=120` → 任务完成后 ≤150s 模块卡变「已停止」，端口释放；恢复 1800；
4. **抽屉回归**：模块页直跑抽屉全流程与改造前一致（不带新字段的请求行为不变）；
5. **任务页**：直跑徽章 + 类型筛选 + `?focus=` 高亮；
6. **i18n**：切 English 无缺键裸串；zh/en 键集合 parity 脚本通过；
7. **异常路径**：未知 module_id 400、双输入缺失 400、队列满 429 有 toast；
8. **浏览器控制台**：全部页面零报错（仓库惯例）。

---

## 9. Phase 2 backlog（本次不做）

- 多文件批量提交（客户端循环 + 进度聚合视图）；
- `GET /api/modules` 暴露 `last_active_epoch_secs` → 模块卡/目录实时空闲倒计时徽章；
- 调用参数预设保存 / 最近使用排序；
- `/run` 会话任务历史 localStorage 持久化；
- CONFIG_REFERENCE 其余滞后字段（staging_* 等）一并补齐。

---

## 10. 风险与缓解

| # | 风险 | 缓解 |
|---|---|---|
| R1 | FE-C 按 §5.3 盲写 import 与 FE-A 实际导出漂移 | 契约含精确签名；W3 `tsc -b` 必抓；INT 裁决并回写本文档 |
| R2 | `lazy_start` 使冷启动失败呈现为 failed 任务（现状是提交时 fast-fail） | 属预期语义变化且仅 `/run` 启用；UI 文案明确"启动失败请在任务页查看原因" |
| R3 | `input_text` 物化文件在 uploads 残留 | 与 v1 门面现状同口径（uploads 清理策略既有）；记入已知限制 |
| R4 | 并行期 i18n 漏译/撞键 | W0 一次性全量预置 + W3 parity 脚本 |
| R5 | 首次运行 venv 准备耗时数十分钟被误判"卡死" | 任务节点状态 + 模块日志深链；文案沿用抽屉既有预期管理话术 |

---

## 11. 待确认决策点

| # | 问题 | 推荐 |
|---|---|---|
| QD1 | 页面名与路由：「快速调用」`/run`？（备选：工具箱 `/toolbox`） | ✅ 快速调用 `/run` |
| QD2 | `lazy_start` 仅 `/run` 默认开启，抽屉维持现状？ | ✅ 是 |
| QD3 | 文本输入采用服务端物化 `input_text`（而非前端 Blob 包装上传的零后端方案）？ | ✅ 服务端物化 |
| QD4 | 批量多文件推迟 Phase 2？ | ✅ 是 |
| QD5 | 空闲倒计时实时徽章推迟 Phase 2（本次仅静态提示）？ | ✅ 是 |
