# scripts/examples — 外部自动化示例脚本

> 决策 6 交付物（PACK_UNIFY_PLAN §6.5「管线即 API」）：EntryPoint 本期不内建
> 文件夹监控 / cron 触发器，无人值守场景由**外部 watcher 进程**承担——
> 发现素材 → 调 API 提交。完整契约与集成模式见
> [docs/AUTOMATION.md](../../docs/AUTOMATION.md)。

## 文件清单

| 文件 | 平台 | 说明 |
|---|---|---|
| `watcher-linux.sh` | Linux | bash + inotifywait：监控目录新文件 → `POST /api/pipelines/execute`（inputs 覆盖 + `wait:false` 异步提交） |
| `watcher-windows.ps1` | Windows | PowerShell + FileSystemWatcher：与 Linux 版同款逻辑 |

两版脚本行为一致：

```
新文件落盘（close_write / Created·Renamed）
  → 扩展名过滤 + 静默等待（防半截文件）+ 幂等检查（可选 .done 标记）
  → POST /api/pipelines/execute
      { "pipeline_id": "...", "inputs": { "<输入节点>": { "path": "<绝对路径>" } },
        "wait": false, "callback_url": "..."(可选) }
  → 202 {"task_id": "..."}；进度经 WS / 轮询 / 回调跟踪
```

## 快速上手

Linux：

```bash
sudo apt install inotify-tools curl        # 依赖（RHEL: dnf / Arch: pacman）
EP_WATCH_DIR=/srv/incoming ./watcher-linux.sh
```

Windows：

```powershell
powershell -ExecutionPolicy Bypass -File .\watcher-windows.ps1 -WatchDir D:\incoming
```

常用配置（Linux 环境变量 / Windows 参数，默认值相同）：

| Linux 环境变量 | Windows 参数 | 默认值 | 说明 |
|---|---|---|---|
| `EP_API` | `-Api` | `http://localhost:9800` | daemon API 地址 |
| `EP_PIPELINE` | `-Pipeline` | `video-to-srt` | 管线 id（`config/pipelines/` 内） |
| `EP_INPUT_NODE` | `-InputNode` | `input` | 覆盖 `path` 的输入节点 id |
| `EP_WATCH_DIR` | `-WatchDir` | `./watch` | 监控目录（自动创建） |
| `EP_EXTENSIONS` | `-Extensions` | 常见音视频后缀 | 接受的扩展名，逗号分隔 |
| `EP_SETTLE_SECS` | `-SettleSecs` | `2` | 落盘后静默秒数（防半截文件） |
| `EP_CALLBACK_URL` | `-CallbackUrl` | 空 | 任务终态回调地址（可选） |
| `EP_MARK_DONE` | `-MarkDone` | 关 | 成功后写 `<文件>.done` 标记防重复提交 |

## 完成回调接收骨架（可选）

设置 `callback_url` 后，daemon 在任务终态时 `POST {task_id, status, artifacts}`
（best-effort，失败仅 warn）。最小接收端示例（Python 3，任意平台）：

```python
# callback-receiver.py — python3 callback-receiver.py
from http.server import BaseHTTPRequestHandler, HTTPServer
import json

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        print("任务终态:", json.dumps(data, ensure_ascii=False, indent=2))
        self.send_response(200)
        self.end_headers()

HTTPServer(("0.0.0.0", 8090), Handler).serve_forever()
```

 watcher 侧填 `EP_CALLBACK_URL=http://localhost:8090/`（Linux）或
`-CallbackUrl http://localhost:8090/`（Windows）即可。

## 要点与边界

- **输入路径是 daemon 侧本地路径**：watcher 与 daemon 同机，或走共享挂载 /
  先 `POST /api/upload/input` 上传；
- **模块自动拉起**：管线引用的模块未运行时 daemon 自动启动并等健康，watcher
  无需关心模块状态；
- **排队**：全局 `max_parallel` + 管线级 `max_instances` 闸门，超额提交自动
  `queued`，watcher 无需限流；
- **认证边界**：API 当前无认证（仅内网 IP 过滤），公网暴露前需认证机制——
  请只在可信内网运行 watcher。

## 另见

- [docs/AUTOMATION.md](../../docs/AUTOMATION.md) — 自动化集成指南（API 契约、
  任务跟踪、回调语义、安全边界）
- [docs/PIPELINE_SPEC.md](../../docs/PIPELINE_SPEC.md) — 管线定义与节点开发
- [docs/PACK_AUTHORING.md](../../docs/PACK_AUTHORING.md) — 整合包作者指南
  （配套 CLI：`ep-pack`，随 GUI/服务器包附带）
