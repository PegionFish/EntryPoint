# 自动化集成指南 (Automation Guide)

> 适用于 EntryPoint v0.x | 契约依据：PACK_UNIFY_PLAN §6.5（管线即 API）与决策 6（watcher 示例）

本文档面向**外部系统集成方**：如何用 HTTP API 把 EntryPoint 当作无人值守的
媒体/模型处理后端——提交执行、跟踪任务、接收回调，以及 watcher 类触发器的
接入模式。

---

## 1. 总览

```
外部系统（watcher / 定时任务 / 业务后端）
   │  ① 发现新素材（文件系统事件 / 业务触发）
   │  ② POST /api/pipelines/execute（或 /api/execute/single）
   ▼
EntryPoint daemon（/api，默认 :9800）
   │  ③ 模块未运行 → 自动拉起并等健康
   │  ④ DAG 调度执行（任务/产物/进度全套复用）
   ▼
结果回流：轮询 GET /api/tasks/... | WS progress | callback_url 回调
```

无人值守三件套（§6.5 契约）：

| 能力 | 契约 |
|---|---|
| **模块自动拉起** | execute/single 提交时引用模块未运行 → 自动启动并等健康；超时计入任务错误 |
| **同步模式** | execute 请求可选 `wait: true` → 阻塞至终态，响应直接带 status + artifacts |
| **完成回调** | 可选 `callback_url` → 终态时 `POST {task_id, status, artifacts}`（best-effort，失败仅 warn） |

---

## 2. 提交执行

### 2.1 `POST /api/pipelines/execute` — 管线执行

```jsonc
{
  "pipeline_id": "video-to-srt",          // 二选一：config/pipelines/ 下已保存管线
  // "spec": { ... },                     // 二选一：前端画布形状的管线 spec
  "inputs": {                              // 可选：按节点覆盖 params
    "input": { "path": "D:/Videos/new/ep-001.mp4" }
  },
  "wait": false,                           // 可选：true = 同步阻塞至终态
  "callback_url": "http://10.0.0.5:8080/ep/callback"  // 可选：终态回调
}
```

- `pipeline_id` 与 `spec` **二选一**（同时给/同时缺 → 400）；
- `inputs`：`node_id → 参数对象`，覆盖管线节点 params（最常用：覆盖
  `file_input` 节点的 `path`）；引用未知节点 → 400；
- 默认异步：`202 {"task_id": "..."}`，进度走 WS / 轮询；
- `wait: true`：阻塞至任务终态，响应直接携带 `status` 与 artifacts 清单
  （内部超时上限取管线超时配置）——适合不想轮询的简单脚本；
- `callback_url`：任务到达终态（completed/failed/cancelled）时 daemon 向该 URL
  `POST {task_id, status, artifacts}`；回调失败只记 warn，不影响任务本身；
- 提交路径自动接入**模块自动拉起**：管线引用的模块未运行 → 先启动并等健康。

### 2.2 `POST /api/execute/single` — 单能力直跑

免开管线，直接执行模块的一个 capability（内部编译为退化三节点 DAG：
file_input → module → file_output，任务/产物链路完全复用）：

```jsonc
{
  "module_id": "faster-whisper",
  "capability": "transcribe",
  "params": { "language": "zh", "timestamps": true },   // 可选，按 capability schema 校验
  "input_path": "D:/Audio/meeting.wav"                  // 服务器本地文件
}
```

- 响应 `202 {"task_id": "..."}`；
- `params` 按模块 manifest 的 capability schema 校验：必填缺失 / 类型不符 /
  枚举越界 → 400（错误文案见 `apiPipelines.single.*` i18n 键）；缺省参数自动
  注入 schema default；
- `input_path` 必须是 daemon 可见的**服务器本地路径**；浏览器侧可先经
  `POST /api/upload/input` 落盘再引用返回路径；
- 模块未运行 → 自动拉起并等健康（超时 → `apiPipelines.single.autostartTimeout`）。

### 2.3 `POST /api/upload/input` — 直跑输入上传

multipart 单文件（字段名 `file`）→ 暂存 `workspace/uploads`，响应
`{"path": "..."}`。把返回路径填给 `/api/execute/single` 的 `input_path` 即可。

---

## 3. 任务跟踪

### 3.1 轮询（REST）

| 端点 | 说明 |
|---|---|
| `GET /api/tasks` | 任务列表（支持状态过滤） |
| `GET /api/tasks/{task_id}` | 任务详情（status / 节点状态 / 错误） |
| `GET /api/tasks/{task_id}/artifacts` | 产物清单 |
| `GET /api/tasks/{task_id}/artifacts/{node_id}` | 下载指定节点产物 |
| `POST /api/tasks/{task_id}/cancel` | 取消任务（已终结 → 409） |
| `GET /api/pipelines/{id}/tasks?status=&limit=` | 某管线的执行历史/在跑任务（含 queued/队列位置） |

任务状态机要点：`queued`（等待全局 `max_parallel` 或管线级 `max_instances`
闸门）→ `running` → `completed` / `failed` / `cancelled`。并发提交自动排队，
无需客户端限流。

### 3.2 WebSocket（实时进度）

`/ws` 的 `progress` 消息携带 `task_id`（+ `pipeline_id`），多任务并发时可据此
过滤；导入类后台任务另有 `pack_import` 消息类型。WS 适合 UI；纯后端集成推荐
轮询或 callback。

---

## 4. watcher 外部触发模式（决策 6）

**EntryPoint 本期不内建触发器**（文件夹监控 / cron 列为后续方向）；无人值守
场景由**外部 watcher 进程**承担，它只做两件事：发现素材 → 调 API 提交。

推荐形态：

```
[watcher]  监听目录（inotify / FileSystemWatcher）
   │  新文件落盘（建议：按扩展名过滤 + 短暂静默防半截文件）
   │  POST /api/pipelines/execute
   │     { "pipeline_id": "video-to-srt",
   │       "inputs": {"input": {"path": "<新文件绝对路径>"}},
   │       "callback_url": "http://localhost:<watcher>/done" }   （可选）
   ▼
[watcher]  收到回调/轮询到 completed → 取产物 → 后处理（归档/通知/入库）
```

要点：

- watcher 与 daemon 同机时，`inputs` 里的路径就是本机路径，直接可用；跨机器
  需先把素材上传（`POST /api/upload/input`）或挂载共享目录；
- 幂等建议：watcher 自行记录已提交文件（如 `.done` 标记文件），避免重启重复提交；
- 失败重试：任务 `failed` 时错误信息在任务详情里（用户可见文案走 i18n）；
  watcher 按业务策略决定是否重提。

**示例脚本**：`scripts/examples/`（Wave 4 D3 交付：Linux bash + inotifywait 与
Windows PowerShell + FileSystemWatcher 双版本，含提交与回调接收骨架）。本节
链接届时补充具体文件清单。

---

## 5. 边界与安全声明

- **输入路径**：所有 `path` / `input_path` 均为**服务器本地路径**；跨机器先
  上传或挂载；
- **认证**：API 当前**无认证**，仅 IP 过滤（`[server] allow_public = false` 时
  只放行 RFC 1918 私有地址）。**公网暴露前必须先落地认证机制**（后续工作，
  本期不做）——请仅在可信内网使用；
- **并发**：全局 `[pipeline] max_parallel` + 管线级 `max_instances` 两级闸门，
  超额提交自动进 `queued` 排队，客户端无需自建限流；
- **回调语义**：`callback_url` 为 best-effort（daemon → 你的 URL 的出站请求），
  注意你的回调端点在 daemon 网络视角下可达；
- **同步模式超时**：`wait: true` 的阻塞上限取管线超时配置，长任务建议
  异步 + 回调。

---

## 6. 快速上手（curl 示例）

```bash
# 1) 已保存管线 + 覆盖输入路径（异步）
curl -X POST http://localhost:9800/api/pipelines/execute \
  -H 'Content-Type: application/json' \
  -d '{"pipeline_id":"video-to-srt","inputs":{"input":{"path":"D:/Videos/a.mp4"}}}'
# → 202 {"task_id":"..."}

# 2) 单能力直跑
curl -X POST http://localhost:9800/api/execute/single \
  -H 'Content-Type: application/json' \
  -d '{"module_id":"faster-whisper","capability":"transcribe","params":{"language":"zh"},"input_path":"D:/Audio/a.wav"}'

# 3) 轮询任务与产物
curl http://localhost:9800/api/tasks/<task_id>
curl http://localhost:9800/api/tasks/<task_id>/artifacts
curl -OJ http://localhost:9800/api/tasks/<task_id>/artifacts/<node_id>
```
