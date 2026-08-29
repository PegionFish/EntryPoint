# EP 触发器机制 + 内置节点扩展 + 统一事件日志 —— 完整执行计划

> **文档性质**：可直接交给执行模型（多子代理编排器）实施的操作文档。
> **冻结级别**：第 2 节决策与第 5 节契约经多轮需求讨论确认，实现时**不得更改**；
> 各子代理的 prompt 必须原样包含其职责相关的契约段落，禁止各自假设接口形状。

---

## 1. 需求背景

用户希望在 EP WebUI 中配置**条件触发机制**：监控 BT 下载目录 / 文件导出目录，
文件出现更新或变动时自动执行处理。讨论中扩展出四项配套需求：

1. 触发器必须是**独立功能**（独立页面、独立规则），不是管线的附属——简单需求
   （如「提取音频」）不应被迫先造管线；
2. 无人值守场景的**产物命名/存储规范**必须有一等公民级的解决方案；
3. 管线需要**条件判断能力**（文件符合标准才处理），也便于后续 API 集成；
4. **所有日志统一保存，保留期由用户决定**；并顺带整理平台既有 Log 能力。

极端场景约束：存在「总文件六位数、文件夹近 1 万、全为图片」的目录——
索引/扫描机制的内存占用必须与目录规模**解耦**。

---

## 2. 已确认决策（多轮讨论结论，禁止更改）

| # | 决策点 | 结论 |
|---|---|---|
| D1 | 数据模型 | 独立触发规则（`rule_id` 为键），同一管线可挂多条规则；集合式 API |
| D2 | 页面形态 | 顶级导航「触发器」页面（规则列表 + 新建/编辑对话框） |
| D3 | 输入注入 | 管线模式**始终手动选择** file_input 注入节点（下拉） |
| D4 | 子目录 | 规则级「递归扫描」开关，默认关 |
| D5 | 存量文件 | 默认不回灌（创建时 `checkpoint=now`）；可选「含存量文件」开关，开启后从最旧文件按 mtime 有序追赶，内存有界 |
| D6 | 洪泛处理 | 不跳过活跃管线，检测即提交入队、慢慢跑；仅保留 256 在途硬上限背压（`QueueFull` 时保持待触发、下轮重试，不丢文件） |
| D7 | 简单需求 | 直接模式复用 `/api/execute/single` 的 ad-hoc 直跑（**全能力清单**，随模块安装自动扩展）+ 内置「仅归档」动作；**不做动作菜单适配** |
| D8 | 产物规范 | 新内置节点 `file_archive`（命名模板 + 冲突策略）；直调产物由 daemon 用同款归档逻辑投递 |
| D9 | 条件判断 | 新内置节点 `file_gate`（单一节点收敛全部过滤语义，不再新增杂项内置节点） |
| D10 | 触发记录/日志 | 统一事件日志设施（`runtime/logs/events-*.jsonl`），保留天数用户决定（0=永久）；触发记录不私嵌规则 |
| D11 | 既有 Log 整理 | 本期同步盘点并归拢既有日志能力（见 §4），补齐 `task_terminal` 事件 |

---

## 3. 总体架构

```
[触发器页（新顶级页面）]
   │ CRUD 规则
   ▼
/api/watchers ──► runtime/watchers.json（规则注册表，原子落盘）
                       │
        watcher 巡检循环（main.rs，10s tick）
                       │ 扫描引擎：checkpoint 水位线 + 在途稳定表
                       │ （内存 = O(新文件到达速率)，与目录总量解耦）
                       ▼
        ┌──────────────┴──────────────┐
   管线模式                        直接模式
   submit_pipeline_for_schedule    ad-hoc 直跑物化（退化 DAG，
   （既有入口，零改动）              末端替换为 file_archive）
        └──────────────┬──────────────┘
                       ▼
        既有执行链路（两级闸门 / FIFO / 256 上限 / 任务注册表）
                       ▼
        统一事件日志 runtime/logs/events-YYYY-MM.jsonl
        （watcher_trigger / task_terminal 事件；保留期巡检清理）
```

管线图内的自动化范式：`file_input → file_gate →（ffmpeg / 模块）→ file_archive`。

---

## 4. 既有 Log 能力盘点与归拢（本期同步整理）

### 4.1 现状盘点

| 能力 | 现状 | 存储 | 保留策略 |
|---|---|---|---|
| daemon 运行日志 | `tracing` 输出（`logging.rs`：RUST_LOG > `general.log_level`，支持运行期热调级） | 控制台 / systemd journal | 由宿主管理（journal） |
| 模块进程日志 | daemon 捕获子进程 stdout/stderr；main.rs 1s 监控循环广播增量行（快照后缀去重）；WebUI 日志抽屉展示（WEBUI_GUIDE §4.1） | `runtime/logs/<module>*.log` | **无**（无限增长） |
| 任务生命周期 | 任务注册表原子落盘，重启非终态改判 failed | `runtime/tasks/` | 无 |
| schedule 触发记录 | 仅 `tracing::info` 日志 + 条目内 `last_task_id` | 无结构化存储 | — |
| 管线完成回调 | `callback_url` 终态 POST（best-effort） | 无 | — |

### 4.2 本期归拢动作

| 动作 | 说明 |
|---|---|
| **新建统一事件日志** | `runtime/logs/events-YYYY-MM.jsonl`（详见 §5.7）；首期事件类型 `watcher_trigger` + `task_terminal` |
| **任务生命周期归一** | `task_terminal` 事件本期**实际接入**（不再仅预留）：任务到达终态（completed/failed/cancelled）时，由执行链路 `finalize` 收尾处统一写一条事件（含 task_id、pipeline_id、状态、错误摘要）。这是「整理既有能力」的核心落点——任务成败从此有统一可查的结构化记录 |
| **模块日志纳入保留策略** | `runtime/logs/` 下的模块日志文件与事件日志文件同受「日志保留天数」清理巡检约束（按文件 mtime） |
| **daemon 控制台日志** | 维持控制台 / journal 输出不变（不引入文件落盘，避免与 systemd 双写）；在 `docs/CONFIG_REFERENCE.md` 明确记载该策略 |
| **触发记录去私嵌化** | 规则内仅保留最近 5 条 `recent` 环形缓冲供列表速览，全量记录在统一事件日志 |
| **文档整理** | `docs/WEBUI_GUIDE.md` 与 `docs/CONFIG_REFERENCE.md` 新增「日志体系」一节：统一列表所有日志类型、位置、查看方式、保留策略配置 |

---

## 5. 冻结契约（所有子代理逐字遵守）

### 5.1 触发规则数据模型

存储：`runtime/watchers.json`（`rule_id → WatchRule`），tmp+rename 原子落盘，
`load` 失败按空表启动并告警——逐行复刻 `crates/ep-daemon/src/schedule.rs` 的持久化模式。

```rust
pub struct WatchRule {
    pub id: String,                                  // 服务端生成，8 位小写十六进制
    pub name: String,
    #[serde(default = "default_true")] pub enabled: bool,
    pub watch_dir: String,                           // 绝对路径
    #[serde(default)] pub recursive: bool,           // 递归子目录
    #[serde(default)] pub extensions: Vec<String>,   // 空=全部；小写无点
    #[serde(default)] pub include_modified: bool,    // 默认仅新文件
    #[serde(default = "default_stability")] pub stability_secs: u64, // 默认 30
    #[serde(default)] pub backfill: bool,            // 含存量文件
    #[serde(default)] pub checkpoint: i64,           // 水位线（epoch 秒）
    #[serde(default)] pub in_flight: HashMap<String, FileSig>, // 在途稳定表（有界）
    #[serde(default = "default_max_batch")] pub max_batch: usize, // 默认 16
    #[serde(default, skip_serializing_if = "Option::is_none")] pub direct: Option<DirectAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub pipeline: Option<PipelineAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub output: Option<OutputConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub last_task_id: Option<String>,
    #[serde(default)] pub recent: VecDeque<EventRecord>, // 最近 5 条速览
}
pub struct FileSig { pub mtime: i64, pub size: u64 }
pub struct DirectAction { pub kind: DirectKind, #[serde(default)] pub params: serde_json::Value }
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DirectKind {
    Archive,                                   // 仅归档（纯搬运重命名）
    Module { module_id: String, capability: String },
}
pub struct PipelineAction { pub pipeline_id: String, pub input_node: String }
pub struct OutputConfig {
    pub dest_dir: String,                      // 绝对路径
    #[serde(default = "default_template")] pub name_template: String, // 默认 "{name}.{ext}"
    #[serde(default)] pub on_conflict: ConflictPolicy,                // 默认 Suffix
}
pub enum ConflictPolicy { Suffix, Overwrite, Skip }
pub struct EventRecord {
    pub ts: i64, pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub task_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
}
```

**命名模板占位符**：`{name}`（无扩展名文件名）、`{ext}`（小写扩展名）、
`{date}`（YYYYMMDD）、`{datetime}`（YYYYMMDD-HHMMSS）、`{rule}`（规则名）、
`{seq}`（冲突序号）。默认模板 `{name}.{ext}`。

### 5.2 扫描语义（纯函数，无副作用，持久化由调用方决定）

函数：`collect_watch_events(registry, now_epoch_secs) -> (Vec<WatchHit>, WatchRegistry)`，
`WatchHit { rule_id, path, rule 快照 }`。

1. **停用规则**：完全跳过（不扫描、不推进）。重新启用后首轮不触发存量。
2. 遍历目录（按 `recursive` 决定递归）收集常规文件 → 过滤：
   - `IGNORED_SUFFIXES` 黑名单：`.part` `.tmp` `.download` `.!qB` `.crdownload` `.bc` `.td` `.xltd`
   - `extensions` 非空时白名单过滤
3. **仅保留 `mtime > checkpoint` 且 `mtime <= now - stability_secs` 的候选**
   （十万存量文件天然被水位线排除，不进入任何索引结构）。
4. 候选判定：
   - 在 `in_flight` 且签名一致 → **稳定，产出触发**；
   - 不在 `in_flight` → 写入（`{mtime, size}`），下轮再判；
   - 签名变化 → 更新签名；后续是否可触发由 `include_modified` 决定
     （`false` 时该文件视为已解决，永不触发）。
5. 触发按 mtime 升序，截断 `max_batch`。
6. `checkpoint` 推进规则（由调用方在提交成功后执行）：推进到**已解决前缀**的最大
   mtime——「已解决」= 已提交触发 或 被过滤规则排除 或 `include_modified=false`
   下签名变化；`in_flight` 中未决文件阻塞其后水位推进。触发/解决的文件从
   `in_flight` 移除；目录中已消失的 `in_flight` 条目剪除。
7. **backfill 哨兵**：`backfill=true` 且 `checkpoint==0` 时，首轮将 `checkpoint`
   置为目录现存候选文件的最旧 mtime，之后按正常流程有序追赶（每轮仅前沿批次进
   `in_flight`，内存有界）。`backfill=false` 时创建即 `checkpoint=now`。
8. 目录缺失/不可读 → `tracing::warn!` 跳过，不拖垮循环（复刻 `collect_due` 容错）。

### 5.3 REST API

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/watchers` | GET | 全部规则列表（含 `recent` 速览） |
| `/api/watchers` | POST | 创建；body 不含 id，服务端生成（8 位小写十六进制），返回 `{ok, id}` |
| `/api/watchers/{id}` | GET | 未配置 → 404 `apiCore.watcher.notFound` |
| `/api/watchers/{id}` | PUT | 全量更新（校验同 POST） |
| `/api/watchers/{id}` | DELETE | 删除，返回 `{ok: true}` |
| `/api/events` | GET | 事件查询：`?rule=<id>&type=<事件类型>&limit=<N，默认100>`，倒序 |

**错误响应统一走 `err_response`（api/mod.rs 既有件）+ i18n 键。**

**POST/PUT 校验顺序**（键前缀 `apiCore.watcher.*`）：
1. `name` 非空 → `nameRequired`
2. `watch_dir` 非空 → `watchDirRequired`；非绝对路径 → `watchDirNotAbsolute`（目录暂不存在仅告警不拒绝）
3. `direct` / `pipeline` 必须恰好一个 → `actionRequired` / `actionConflict`
4. pipeline 模式：管线存在（复用 `find_spec_file`，否则 404 `apiPipelines.pipelines.notFound`）且 `input_node` 为 spec 中实际节点 → 否则 `inputNodeInvalid`
5. direct-Module：模块与 capability 存在（复用 modules 注册表）→ 否则 `capabilityInvalid`
6. direct 模式：`output` 必填且 `dest_dir` 绝对 → `outputRequired`
7. `stability_secs` 钳制 ≥ 5；`extensions` 去点转小写归一

### 5.4 触发提交路径

| 规则模式 | 提交方式 | 任务 `pipeline_id` |
|---|---|---|
| 管线模式 | 复用 `execution.rs::submit_pipeline_for_schedule`（**零改动**），inputs = 规则模板 ∪ `{input_node: {"path": "<绝对路径>"}}` ∪ `_meta: {"rule": 规则名}` | 真实管线 id |
| 直调-仅归档 | 物化两节点退化 DAG（`file_input → file_archive`），file_archive params 由规则 `output` 渲染，走 `submit_pipeline_full` | `watcher/<rule_id>` |
| 直调-模块 | 仿 `execute_single` 流程：先 `ensure_module_running`（api/autostart.rs），再物化退化三节点 DAG（`file_input → module → file_archive`），末端 file_archive 同上 | `watcher/<rule_id>` |

提交结果记事件日志（`watcher_trigger`）：成功 `submitted`；`QueueFull` 等失败
`rejected`（文件保持待触发，下轮重试）；仅归档冲突策略 `skip` 命中时
`archive_skipped`。规则 `recent` 与 `last_task_id` 同步回写。

### 5.5 内置节点 `file_archive`（ep-core）

- **参数**：`dest_dir`（必填，绝对）、`name_template`（默认 `{name}.{ext}`）、
  `on_conflict`（`suffix`/`overwrite`/`skip`，默认 `suffix`）。
- **占位符源**：`{name}/{ext}/{date}/{datetime}/{seq}` 自上游文件名与当前时间；
  `{rule}` 经任务 inputs 保留键 `_meta.rule` 注入（普通手动任务缺省空串）。
- **行为**：取上游首个 `Artifact::File` → 渲染目标路径 → 冲突策略 → 复制落盘 →
  返回 `Artifact::File(dest)`。`skip` 命中时正常完成（不视为错误，记日志）。
- **实现要求**：模板展开 + 冲突处理为独立纯函数模块 `crates/ep-core/src/archive.rs`
  （供节点执行与直调物化共用），≥8 单测覆盖占位符/冲突/非法模板。
- **端口类型**：文件入 → 文件出（`validate()` 兼容矩阵：任意→文件已支持）。

### 5.6 内置节点 `file_gate`（ep-core）

- **参数**（全部可选，`validate()` 要求至少配置一项，否则静态校验报错）：
  - `extensions`（白名单）、`extensions_exclude`（黑名单）
  - `min_size_bytes` / `max_size_bytes`
  - `filename_regex`（Rust regex）
  - `media`：`{min_duration_secs?, max_duration_secs?, min_width?, min_height?}`，
    经 `ffprobe -v error -print_format json -show_format -show_streams` 探测；
    探测失败按「不满足」处理（运行期容错，不在静态校验强制）
  - `on_mismatch`：`skip`（默认）/ `fail`
- **满足** → 透传上游文件。
- **不满足**：
  - `fail`：节点返回错误（走既有 失败→下游 Skipped 语义）。
  - `skip`：**引擎增强** —— `Artifact` 新增 `None` 变体；`file_gate` 返回
    `Artifact::None`；runner 对「全部输入均为 `Artifact::None`」的下游节点置
    `NodeState::Skipped`；任务终态 Completed，但任务详情标记「无匹配输出」。
- **核心难点**：`Artifact::None` 传播不得破坏既有失败/取消/同层兄弟语义。
  实现代理**必须先通读** `crates/ep-core/src/pipeline/runner.rs` 层推进逻辑
  （`skip_layer_remaining`、`check_completion`、fail-fast 分支）再动手，并补
  专项回归：gate 跳过 / gate 失败 / 取消叠加 / 多分支部分满足。

### 5.7 统一事件日志

- **文件**：`runtime/logs/events-YYYY-MM.jsonl`，单行 JSON 追加（`write_all` + flush），
  按月滚动；读路径容忍尾行不完整（解析失败跳过该行）。
- **事件形状**（公共字段 `ts`、`type`）：

| type | 字段 | 写入方 |
|---|---|---|
| `watcher_trigger` | rule, file, task_id?, status∈{submitted, rejected, archive_done, archive_skipped}, detail? | watcher 巡检循环 |
| `task_terminal` | task_id, pipeline_id, status∈{completed, failed, cancelled}, error? | 执行链路任务终态收尾处（`finalize` 统一出口） |

- **读取**：`GET /api/events` 倒序（从最新月份文件向前），支持 `rule`/`type`/`limit` 过滤。
- **保留策略**：`config/app.toml` `[general]` 新增 `log_retention_days`（整数，
  默认 90，**0=永久**）；`ep-core/src/config.rs` 加字段（`#[serde(default = "default_90")]`）；
  设置页输入项；`PUT /api/config` 既有合并落盘机制天然支持。
- **清理巡检**（main.rs，1 小时 tick）：`log_retention_days > 0` 时，删除
  `runtime/logs/` 下 mtime 超期的 `events-*.jsonl` 与模块日志文件；=0 时跳过。

### 5.8 i18n 键（硬约束：禁止硬编码界面文案，en 与 zh-CN 对齐）

| 文件 | 键组 | 内容 |
|---|---|---|
| `i18n/locales/{en,zh-CN}/triggers.json`（**新建**） | 触发器页全部文案 | 页面标题/列表表头/对话框字段/动作模式/输出配置/历史/空态/错误提示 |
| `i18n/locales/{en,zh-CN}/apiCore.json` | `watcher.*` | nameRequired / watchDirRequired / watchDirNotAbsolute / actionRequired / actionConflict / inputNodeInvalid / capabilityInvalid / outputRequired / notFound / saveFailed |
| `i18n/locales/{en,zh-CN}/pipeline.json` | `nodes.fileArchive.*`、`nodes.fileGate.*` | 编辑器节点面板、参数标签与提示 |
| `i18n/locales/{en,zh-CN}/settings.json` | `logRetention*` | 保留天数输入标签与说明 |

前端新命名空间 `triggers` 需在 `frontend/src/i18n/index.ts` 注册（扁平键、
`keySeparator: false`，与既有 9+ 命名空间同款约定）。

---

## 6. 执行波次与子代理拆分

### 总览

```
Wave 0（3 并行）:  W0-A ep-core 节点   ∥  W0-B 事件日志   ∥  W0-C watcher 核心
                         └────────────────┬────────────────┘
Wave 1（4 并行）:  W1-D 直调物化 ∥ W1-E daemon 集成 ∥ W1-F 前端触发器页 ∥ W1-G 前端编辑器+设置
                         └────────────────┬────────────────┘
Wave 2（3 并行）:  Rust 验证 ∥ 前端验证 ∥ 日志设施验证
Wave 3:           Browser E2E（单代理）
Wave 4（3 并行）:  CodeReview×3（完整性 / 正确性 / 影响面，各只评一维）
收尾:             定向修复（如需）→ 清理临时文件与进程
```

### Wave 0 —— 基础三路并行（同一回合发出 3 个 Coding 子代理）

#### W0-A ｜ ep-core：file_archive + file_gate 节点

- **目标**：两个新内置节点 + `Artifact::None` 引擎增强，既有测试零回归。
- **文件**：`crates/ep-core/src/archive.rs`（新）、`pipeline/executor.rs`、
  `pipeline/runner.rs`、`pipeline/validate.rs`、Artifact 定义所在文件、相关测试文件。
- **输入**：§5.5、§5.6 全文；`runner.rs` 通读先行（强制）。
- **步骤**：① `archive.rs` 纯函数（模板展开/三策略冲突处理，≥8 单测）→
  ② `file_archive` 节点执行分支 → ③ `Artifact::None` + `file_gate` 执行（含
  ffprobe 探测）→ ④ runner 跳过传播与 `check_completion`「无匹配输出」标记 →
  ⑤ `validate()` 静态校验（file_gate 至少一项条件；两节点端口类型）。
- **完成标准**：`cargo test -p ep-core` 全绿（含既有全部测试）；专项回归覆盖
  §5.6 列出的四类叠加场景。

#### W0-B ｜ 统一事件日志设施

- **目标**：事件日志写/读/滚动/清理 + 保留期配置 + `GET /api/events` + `task_terminal` 接入。
- **文件**：`crates/ep-daemon/src/eventlog.rs`（新）、`crates/ep-core/src/config.rs`、
  `config/app.toml`、`crates/ep-daemon/src/api/events.rs`（新）、
  `crates/ep-daemon/src/api/mod.rs`（仅追加 events 路由行，插入位置见 §7 规则）、
  `crates/ep-daemon/src/execution.rs`（**仅**任务终态收尾处新增一行事件写入调用）。
- **输入**：§4.2、§5.7 全文。
- **步骤**：① `eventlog.rs`（按月文件名/追加写/倒序查询/保留期筛选纯函数，≥8 单测）→
  ② config 字段 + app.toml 缺省 → ③ `api/events.rs` → ④ `task_terminal` 在执行
  链路终态统一出口接入（找 `finalize` 收尾路径，单一接入点，不分散）。
- **完成标准**：`cargo test -p ep-core -p ep-daemon` 全绿。

#### W0-C ｜ watcher 核心（注册表 + 扫描引擎 + API）

- **目标**：触发规则完整后端面（不含巡检循环挂载与提交物化）。
- **文件**：`crates/ep-daemon/src/watcher.rs`（新）、`crates/ep-daemon/src/api/watchers.rs`
  （新）、`api/mod.rs`（仅追加 watchers 路由行）、`i18n/locales/{en,zh-CN}/apiCore.json`。
- **输入**：§5.1–§5.3、§5.8 后端键全文；仿写蓝本 `schedule.rs`（全文）与
  `api/pipelines.rs` 的 schedule 三端点。
- **步骤**：① 数据模型 + 原子落盘 → ② `collect_watch_events` 纯函数，≥10 单测：
  稳定触发 / 半文件后缀过滤 / 静默期未满不触发 / 扩展名过滤 / 停用跳过 / 目录缺失
  容错 / 水位线推进与剪枝 / **存量不回灌** / **backfill 有序追赶且内存有界** /
  重启不重放 → ③ 5 个 API 端点 + 校验链 + i18n 键。
- **完成标准**：`cargo test -p ep-daemon` 全绿；单测须含 10 万文件量级的
  **合成断言**（不必造真文件：以构造的注册表 + mock 文件列表验证水位线排除逻辑）。

### Wave 1 —— 集成四路并行（Wave 0 全部完成并验证后，同一回合发出）

#### W1-D ｜ 直调物化提交（依赖 W0-A 的 file_archive、W0-B 事件日志）

- **文件**：`crates/ep-daemon/src/execution.rs`（**仅新增**直调物化函数区段，
  不动既有函数）。
- **输入**：§5.4。
- **内容**：`submit_direct_archive`（仅归档两节点）与 `submit_direct_module`
  （模块直调：`ensure_module_running` → 退化三节点，末端 file_archive）；
  params 渲染自规则 `output`；`_meta.rule` 注入；`pipeline_id = "watcher/<rule_id>"`。
- **完成标准**：新增函数带单测（退化 DAG 结构断言）；既有直跑/管线测试零回归。

#### W1-E ｜ daemon 集成（依赖 W0-B、W0-C、W1-D）

- **文件**：`crates/ep-daemon/src/main.rs`（两个新循环 + `mod` 声明）、
  `crates/ep-daemon/tests/e2e_daemon.rs`（如需 `#[path]` 模块列表补新文件）。
- **输入**：§5.2（调用方职责）、§5.4、§5.7；仿写蓝本 main.rs L436-501（cron 巡检）。
- **内容**：
  ① **10.7 watcher 巡检循环**：10s `tokio::time::interval` +
  `MissedTickBehavior::Delay`；注册表空即 `continue`；加载 → `collect_watch_events`
  → 逐 hit 按规则模式提交（管线走既有入口，直调走 W1-D 函数）→ 成功回写
  `checkpoint`/`in_flight`/`recent`/`last_task_id` + 写事件日志 → 无状态变化不落盘；
  ② **10.8 日志清理循环**：1h tick，按 §5.7 清理策略执行。
- **完成标准**：`cargo test -p ep-daemon` 全绿；`cargo clippy -p ep-daemon -- -D warnings`。

#### W1-F ｜ 前端触发器页（凭冻结契约并行，不依赖后端完成）

- **文件**：`frontend/src/pages/triggers.tsx`（新）、`App.tsx`、导航组件、
  `api/types.ts`、`api/client.ts`、`i18n/index.ts`、`i18n/locales/{en,zh-CN}/triggers.json`（新）。
- **输入**：§5.1（TS 形状）、§5.3、§5.8；仿写蓝本 `pages/pipeline.tsx` 的
  `ScheduleDialog` 与 `api/client.ts` 的 schedule 三接口。
- **内容**：
  ① 规则列表：名称/监控目录/动作模式/启停开关/最近触发（`recent`）/编辑/删除；
  ② 规则对话框：动作模式切换——直接模式（「仅归档」或 模块→capability 级联下拉，
  能力数据源复用 run 页既有的模块能力接口）+ 输出配置（目标目录/命名模板/冲突策略）；
  管线模式（管线下拉 + 选中后拉取 spec、file_input 节点下拉）；公共区：扩展名过滤、
  递归开关、静默秒数、含存量开关、启用开关；
  ③ 规则详情触发历史：`GET /api/events?rule=<id>` 倒序列表；
  ④ 路由与顶级导航注册；全部文案走 `triggers:` 命名空间 i18n。
- **完成标准**：`npm run build`（tsc + vite）通过；无硬编码界面文案。

#### W1-G ｜ 前端编辑器 + 设置（与 W1-F 文件零交集）

- **文件**：`frontend/src/pages/pipeline.tsx`（节点面板/参数表单）、
  `frontend/src/pages/settings.tsx`、`i18n/locales/{en,zh-CN}/pipeline.json`、
  `settings.json`。
- **输入**：§5.5、§5.6、§5.7 保留期、§5.8 对应键。
- **内容**：① 管线编辑器节点面板登记 `file_archive` / `file_gate`（图标、参数表单、
  端口类型文件入→文件出，仿既有内置节点登记方式）；② 设置页 `log_retention_days`
  输入（0=永久说明文案）。
- **完成标准**：`npm run build` 通过。

### Wave 2 —— 并行验证（Wave 1 全部完成，同一回合发出 3 个 Verify 子代理）

1. **Rust 验证**：`cargo build`（workspace）、`cargo test -p ep-core -p ep-daemon`、
   `cargo clippy -- -D warnings`；确认 `Cargo.lock` 无变化（本期**零新依赖**）。
2. **前端验证**：`cd crates/ep-webui/frontend && npm run build`；grep 检查本期
   改动文件无硬编码中/英界面文案（全部 `t()`）。
3. **日志设施验证**：构造事件 → 校验 jsonl 行格式与按月文件名 → 造超期文件验证
   清理函数 → `GET /api/events` 过滤（rule/type/limit）正确 → 尾行损坏容忍。

### Wave 3 —— Browser E2E（Wave 2 全绿；协调者先后台启动 `cargo run -p ep-daemon`）

Browser 代理执行（每步截图留证）：
1. 导航出现「触发器」页，空态正常；
2. 创建「仅归档」规则：监控临时目录（`runtime/staging`），稳定性 10s，输出目录
   指定另一临时目录、模板 `{name}-{date}.{ext}`、冲突 suffix → 丢入测试文件 →
   约 60s 内确认：文件按模板出现在输出目录；列表页最近触发更新；详情触发历史有
   `submitted`/`archive_done` 记录；
3. 重复丢入同名文件 → 确认冲突序号生效（`-1`）；
4. 创建管线模式规则（内置轻量管线，如 `audio-extract`；选择其 file_input 节点）→
   丢入 `workspace/` 既有媒体文件 → 任务中心出现自动任务；
5. 停用规则 → 丢文件不再触发；重新启用恢复；删除规则后 `watchers.json` 条目消失；
6. 设置页修改保留天数可保存；
7. 清理：删除全部测试规则、临时文件。

### Wave 4 —— 三路并行评审（同一回合发出 3 个 CodeReview 子代理，各只评一维）

| 代理 | 维度 | 重点 |
|---|---|---|
| R1 | 完整性 | 对照 §2 决策表逐项核对；契约字段/端点/键清单全覆盖 |
| R2 | 正确性 | 水位线边界、`Artifact::None` 传播竞态、事件日志并发追加、注册表读改写窗口、backfill 内存断言真实性 |
| R3 | 影响面 | 对 schedule / 执行链路 / 既有四类内置节点 / 直跑 `/api/execute/single` 的回归 |

发现问题 → 派发定向修复代理 → 仅复验受影响范围 → 收尾清理临时文件与后台进程。

---

## 7. 文件所有权矩阵与并行规则（铁律）

| 属主代理 | 独占文件 |
|---|---|
| W0-A | `crates/ep-core/src/archive.rs`（新）、`pipeline/executor.rs`、`pipeline/runner.rs`、`pipeline/validate.rs`、Artifact 定义文件及对应测试 |
| W0-B | `ep-daemon/src/eventlog.rs`（新）、`ep-core/src/config.rs`、`config/app.toml`、`ep-daemon/src/api/events.rs`（新）、`api/mod.rs` events 行、`execution.rs` **仅**终态写入行 |
| W0-C | `ep-daemon/src/watcher.rs`（新）、`api/watchers.rs`（新）、`api/mod.rs` watchers 行、`i18n/*/apiCore.json` |
| W1-D | `ep-daemon/src/execution.rs` 新增直调物化区段 |
| W1-E | `ep-daemon/src/main.rs`、`tests/e2e_daemon.rs`（如需） |
| W1-F | `frontend/src/pages/triggers.tsx`（新）、`App.tsx`、导航组件、`api/types.ts`、`api/client.ts`、`i18n/index.ts`、`i18n/*/triggers.json`（新） |
| W1-G | `frontend/src/pages/pipeline.tsx`、`pages/settings.tsx`、`i18n/*/pipeline.json`、`settings.json` |
| W2-H（文档，可与 Wave 1 并行） | `docs/AUTOMATION.md`、`docs/WEBUI_GUIDE.md`、`docs/PIPELINE_SPEC.md`、`docs/CONFIG_REFERENCE.md` |

**并行规则**：
1. 任一时刻每个文件单一属主；跨文件依赖一律通过本文件冻结契约衔接，不得口头约定。
2. `api/mod.rs` 双代理触碰规则：W0-C 的 watchers 路由行追加于现有末行之后，
   W0-B 的 events 行插入在 W0-C 行之前；冲突由 Wave 2 统一修复。
3. `execution.rs` 双代理触碰规则：W0-B 只加终态写入行（既有函数体内一行），
   W1-D 只加新函数区段；Wave 1 串行关系（W1-D 在 Wave 1，W0-B 在 Wave 0）天然错开。
4. 前端两代理文件零交集；`i18n/locales` 按文件分治（各代理只动自己的命名空间文件）。
5. 文档代理（W2-H）只读代码、只写 `docs/`，可与任意波次并行。
6. 全部代理：本期**零新 Cargo 依赖**（`Cargo.lock` 不得变化；`build.sh --locked` 门禁）；
   界面文案**零硬编码**。

**文档代理（W2-H）任务**：
- `AUTOMATION.md` §4：「本期不内建触发器」更新为内建触发器（轮询 10s + 稳定窗口
  + 水位线语义），外部 watcher 脚本降级为跨机/低延迟补充方案；
- `PIPELINE_SPEC.md`：新增 `file_archive` / `file_gate` 内置节点章节（参数表 + 示例图）；
- `WEBUI_GUIDE.md`：触发器页操作指南 + 「日志体系」一节（§4.1 盘点表落地版）；
- `CONFIG_REFERENCE.md`：`log_retention_days` 字段；修订「当前不产生 logs/ 目录」注释。

---

## 8. 测试计划汇总

| 层 | 内容 | 门禁 |
|---|---|---|
| ep-core 单测 | `archive.rs` 模板/冲突 ≥8；file_gate 判定 ≥8；`Artifact::None` 传播回归（跳过/失败/取消/多分支叠加）；既有测试零回归 | `cargo test -p ep-core` |
| ep-daemon 单测 | 扫描引擎 ≥10（含存量不回灌、backfill 内存有界合成断言）；事件日志 ≥8；规则 API 校验链（Router::oneshot，仿 execute.rs 测试风格） | `cargo test -p ep-daemon` |
| 静态检查 | 全 workspace | `cargo clippy -- -D warnings` |
| 前端 | 构建 + 零硬编码文案 | `npm run build` + grep |
| 日志设施 | 落盘格式/滚动/清理/查询/尾行容错 | Wave 2-3 |
| E2E | §Wave 3 七步 | Browser 截图证据 |

---

## 9. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 极端目录（10 万文件/万文件夹）内存与追赶风暴 | 水位线排除存量 + 在途表仅前沿批次 + backfill 按 mtime 有序；残余成本为每轮全目录 `read_dir+stat`（约几百毫秒），间隔后续可配置化 |
| BT 半截文件误触发 | 后缀黑名单 + 跨轮签名一致 + 静默窗口三重防线；文档建议「完成后移动到子目录」模式 |
| `Artifact::None` 传播破坏既有失败/取消语义 | 强制通读 runner 先行；专项叠加回归；既有测试零回归为硬门禁 |
| 事件日志并发写 | 单进程追加 + 单行原子；读路径容忍尾行不完整 |
| 前后端契约漂移 | 本文件契约逐字冻结，各代理 prompt 原样包含；Wave 2 集成验证兜底 |
| 直调模块拉起失败 | 复用 `ensure_module_running` 错误语义；记 `rejected` 事件，文件保持待触发重试 |
| 注册表并发读改写（巡检循环与 API PUT） | 与 schedule 现状一致的读-改-写窗口（单进程、10s 周期），不引入新锁，保持与既有代码同级设计 |
| `task_terminal` 接入点遗漏（多终态路径） | 必须在 `finalize` 类统一出口单点接入；单测断言三条终态路径各产生一条事件 |

---

## 10. 拒绝的备选方案

| 方案 | 拒绝理由 |
|---|---|
| 全量快照索引 | 10 万文件 ≈ 30-50MB 内存 + 20MB JSON 每轮重写 + 二轮齐触发风暴；被水位线方案取代 |
| 触发器作为管线子功能（按管线为键） | 简单需求被迫造管线、一条管线无法挂多目录；被独立规则模型取代 |
| 内置「简单动作菜单」逐个适配 | 与 ad-hoc 直跑重复造轮子；直接复用全能力清单 |
| 增强既有 `file_output` 承载归档 | 改变已验证节点契约，回归面大；新节点更干净 |
| 触发规则直接配输出、私自搬运产物 | 绕开引擎产生两套产物语义；归档节点保持引擎单一出口 |
| 触发历史内嵌规则 JSON | 与「日志统一保存、保留期用户决定」冲突；并入统一事件日志 |
| 多个文件操作类内置节点（重命名/移动等） | 违背「内置节点极简」架构决策；收敛为 file_gate + file_archive |
| notify crate 事件驱动 | 零依赖轮询已满足场景，稳定窗口判定两种方案都绕不开；保留未来增量替换空间 |

---

## 11. 验收清单

- [ ] 「触发器」顶级页面：规则增删改查、启停、历史查看全部可用，双语文案完整
- [ ] 仅归档规则端到端：新文件 → 模板命名落盘 → 冲突序号 → 事件日志可查
- [ ] 直调模块规则：模块自动拉起 → 产物归档；模块未装时错误可见
- [ ] 管线规则：注入节点手动选择；自动任务出现在任务中心
- [ ] 极端场景：存量十万级目录创建规则无内存膨胀、无回灌；backfill 开关有序追赶
- [ ] `file_gate`：skip/fail 两模式；与取消/失败叠加语义正确
- [ ] 统一事件日志：按月滚动、`GET /api/events` 过滤、保留天数（0=永久）生效
- [ ] `task_terminal` 事件：三条终态路径均产生记录
- [ ] 零新依赖（`Cargo.lock` 不变）、零硬编码文案、既有测试全绿
- [ ] 四份文档更新完毕（AUTOMATION / PIPELINE_SPEC / WEBUI_GUIDE / CONFIG_REFERENCE）
