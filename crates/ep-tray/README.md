# ep-tray — EntryPoint Windows 托盘管理器

单 exe 托盘壳（`tray-icon` + `tao`，**零 webview 依赖**），托管同目录的
`ep-daemon.exe` sidecar。壳不承载业务功能：所有业务能力经 WebUI
（默认浏览器打开 `http://127.0.0.1:9800`）。

## 功能

- 托盘图标三色状态：绿=运行中（端口探活通过）/ 黄=启动中·已停止 / 红=异常
- 菜单：状态行（只读）、打开 WebUI、重启 daemon、停止 daemon、
  开机自启开关（HKCU Run 键，`"ep-tray.exe 路径" --minimized`）、
  退出（并停止托盘托管的 daemon）
- daemon sidecar 托管：启动即自动拉起（端口已有 daemon 时采纳，不重复拉起）、
  崩溃自动重启**一次**、再失败转红标停止自动重启（需用户手动介入）
- 单实例锁（Windows 命名互斥体，`Local\EntryPoint.ep-tray.singleton`）：
  双开时第二个实例静默退出

## 参数

| 参数 | 说明 |
|---|---|
| `--minimized` | 静默启动：不自动打开 WebUI（开机自启 Run 键使用） |
| `--port=<n>` | daemon 端口（缺省 9800，与 config `[server] port` 保持一致） |
| `--help` | 用法 |

## 构建

```powershell
cargo build -p ep-tray --release          # x64
cargo build -p ep-tray --release --target aarch64-pc-windows-msvc   # arm64
```

发布部署：把 `ep-tray.exe` 与 `ep-daemon.exe` 放同一目录（sidecar 约定），
双击 `ep-tray.exe` 或在托盘菜单里勾选"开机自启"。

## 编译策略（跨平台）

全部 tray 依赖收在 `[target.'cfg(windows)'.dependencies]`，平台代码
`#[cfg(windows)]` 门控——非 Windows 目标只编译空壳（纯逻辑模块 +
提示退出的 main），Linux/CI 无需 GTK/系统库即可 `cargo check`。
"重启一次/再失败红标"状态机与单实例锁均为 IO 注入的纯逻辑模块
（`src/supervisor.rs` / `src/singleton.rs`），带完整单测。

## 已知限制与风险

- **杀毒软件误报（已知长期风险）**：托盘程序 + 拉起子进程 + 写 Run 键 +
  命名互斥体的组合特征可能被部分杀软（含 Windows Defender 的启发式）
  误报。属误报（全部源码在仓库内可审计）；被隔离时在杀软中恢复并加
  白名单即可。
- "停止 daemon"/退出为强制终止（daemon 无 HTTP 停机端点，控制台信号
  优雅停机不适用于无窗口托盘场景）；运行中的模块子进程由 daemon 自身
  的孤儿回收兜底，建议停机前先在 WebUI 停掉运行中的任务。
- 托管线程只管理**自己拉起**的 daemon：端口上预先存在的外部 daemon
  会被采纳显示，但"停止"对其无效（无子进程句柄）。
