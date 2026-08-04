# EntryPoint 功能完成度审计报告

> 日期：2026-08-04 | 方法：8 个并行审计/侦察代理分区扫描（管线端到端、WebUI 功能面、daemon API 与横切、ep-core/桌面端缺口）
>
> 判定标记：✅ 完整 / ⚠️ 部分可用 / ❌ 缺失或损坏 / 💀 死代码（已声明未接线）

---

## 0. 总览

| 层 | 结论 |
|---|---|
| HTTP API 面（28 端点） | 完成度高，无 stub，错误处理与 i18n 完备 |
| 引擎内核（DAG/builtin 节点/健康检查/下载进度链） | 真实且有测试 |
| **管线编排（用户自然路径）** | ❌ 两处 P0 契约失配，拖模块节点/配 ffmpeg 必挂 |
| **配置兑现率** | ❌ 10+ 配置项无运行时消费者（"改了有用"的假象） |
| **执行语义** | ❌ 零超时、零取消、并发无闸门、任务记录不落盘 |
| **桌面端执行面** | ❌ 模块启动必挂、下载死锁、任务页恒空、管线只读 |
| 设备调度器 | 💀 四策略完整实现但生产零接线 |

---

## 1. P0 — 功能性损坏

| # | 缺陷 | 证据 | 影响 |
|---|---|---|---|
| P0-1 | **管线编辑器：模块 capability 命名失配**。前端硬编码 `asr.transcribe` 式 id（pipeline-node.tsx:269-311），模块路由只认裸名 `transcribe`（adapter.py 404 拒绝）；根因是 `/api/modules` 不暴露 manifest capabilities，前端只能猜 | executor.rs:1097 拼 `/predict/{capability}` | WebUI 拖入的任何模块节点执行必 404 |
| P0-2 | **管线编辑器：ffmpeg args 类型失配**。前端给字符串（pipeline-node.tsx:211），后端只认数组（executor.rs:967）→ 参数被静默丢弃；且前端缺 `output_extension` 字段 | 对照内置 TOML 均为数组 | WebUI 创建的 ffmpeg 节点必挂 |
| P0-3 | **`--run-module` CLI 完全不可用**：① env 键已带 `EP_` 前缀再被 process.rs:166 加一层 → `EP_EP_ROOT`；② `{MODULE_DIR}`/`{venv_python}` 占位符因键名不匹配残留为字面量 | ep-daemon main.rs:117-125 × process.rs:166 | 所有 5 个模块 standalone 启动必败 |
| P0-4 | **桌面端启动模块必挂**：StartModule 传空 env_vars，占位符不替换（同 P0-3 机理） | ep-desktop main.rs:195-200 | 桌面端模块功能实质不可用 |
| P0-5 | **桌面端新装环境模型下载死锁**：下载要求 venv 已存在，但桌面端从不调 ensure_venv，StartModule 也不建 venv | ep-desktop main.rs:251-259 | 全新安装无法下载任何模型 |
| P0-6 | **管线执行零超时零取消**：ffmpeg 子进程无时限；`default_timeout_secs`/节点 `timeout_secs` 全死；无取消路径；`TaskStatus::Cancelled` 全仓无产生方 | executor.rs:1012/1053、pipeline_bridge.rs:197/282 | 长任务无保护，卡死无解 |

## 2. P1 — 宣称有但未接线的核心能力

| # | 缺陷 | 证据 |
|---|---|---|
| P1-1 | **设备调度器整体未接线**：四策略+overcommit+VRAM 记账只有测试在跑；daemon 用"manifest backends 首个匹配"，桌面端用"首个非 CPU"且不看 manifest backends | compute/scheduler.rs × api/modules.rs:232-239 × ep-desktop main.rs:188-192 |
| P1-2 | **模块未启动时管线节点硬失败**："no port registered"，无自动拉起、无提交前预检 | executor.rs:1085-1097、execution.rs:280-286 |
| P1-3 | **`pipeline.max_parallel` 未实现**：配置存在、两端设置页可调，执行路径零闸门 | execution.rs:259 无限 spawn |
| P1-4 | **任务注册表无持久化**：daemon 重启全丢，产物文件在盘上但索引消失 | execution.rs:102 纯内存 |
| P1-5 | **`GET /api/pipelines/{id}/status` 恒 unknown**：查的 state.runner 从未被执行（真实执行每任务自建 runner） | pipelines.rs:282 × execution.rs:340 |
| P1-6 | **桌面端任务页恒空**：无执行管线/拉取任务的 AppCmd，TasksRefreshed 无生产方 | app.rs:407 × AppCmd 枚举 |
| P1-7 | **桌面端模块日志死路**：LogLine 有渲染端，background_loop 从不发送 | app.rs:314 × main.rs |
| P1-8 | **模块子进程代理未注入**：NetworkConfig 文档承诺覆盖"模块子进程"，但 ProcessManager 裸创建，with_network_env 零调用 | config.rs:217-220 × state.rs:166 |
| P1-9 | **PUT /api/config 整体替换陷阱**：请求体缺省字段被默认值静默重置 | api/config.rs:39 |
| P1-10 | **daemon 自身更新检查不存在**：`general.check_updates` 无消费者 | config.rs:114 |
| P1-11 | **节点级 retry_count/timeout_secs 被桥接层丢弃**，仅剩 HTTP 固定重试 1 次 | pipeline_bridge.rs:197、executor.rs:1048 |
| P1-12 | **健康检查超时不杀进程**：Error 态模块继续占端口和显存 | process.rs:296-315 |
| P1-13 | **ROCm/OpenVINO/DirectML 检测器完全缺失**：all_detectors 只有 Cuda+Cpu | compute/mod.rs:20-22 |

## 3. P2 — 死配置 / 一致性 / 次要缺口

| # | 缺陷 | 证据 |
|---|---|---|
| P2-1 | **死配置清单**：`check_updates`、`log_level`（daemon 硬编码 info）、`max_parallel`、`default_timeout_secs`、`keep_workspace`（无清理代码）、`dashboard_refresh_secs`、`max_concurrent_downloads`（下载无并发闸）、`compute.strategy`/`single_device`/`allow_overcommit`（调度器未接线） | config.rs 详见 §6 表 |
| P2-2 | **主题三端不同步**：服务器 theme 只在点保存时写、启动不读服务器、顶栏切换不回写 | settings.tsx/store/theme.ts/header.tsx |
| P2-3 | **模块能力列表无任何展示**：ModuleResponse 无 capabilities 字段（与 P0-1 同根因） | api/types.ts:24-33 |
| P2-4 | **"设备"列恒显"暂不支持"**：dashboard 模块表与模块详情两处空数据位 | dashboard.tsx:296、module-detail.tsx:388 |
| P2-5 | **PortManager 无 OS 级占用探测**：外部占用照分配，冲突等 30s 健康超时才暴露；无落盘 | port.rs:30-63 |
| P2-6 | **下载/任务均无取消端点**（ep-core 句柄就绪；桌面端下载反而有取消） | models.rs 注释 |
| P2-7 | **WS progress 不带 task_id**：并发任务画布状态串染；执行锁依赖 WS，断连永久锁死 | state.rs ProgressMessage × pipeline.tsx:1276 |
| P2-8 | **热更新裂缝**：改 ports/workspace_dir/host/port/refresh_interval 后 PortManager、ServeDir 根、刷新循环不跟随；workspace_dir 改后新任务产物不可下载 | tasks.rs:56-65 |
| P2-9 | **api/modules.rs 硬编码 `root.join("models")`**，不走 config cache_dir | api/modules.rs:244 |
| P2-10 | **模型 size 恒 0**：list_downloaded_models 的 TODO | model.rs:623 |
| P2-11 | **DAG 校验不查漏斗**：孤儿节点、重复边、端口类型不检查；daemon 提交路径不调 validate()（只查环） | dag.rs:161-199、execution.rs:259 |
| P2-12 | **层内节点串行**，与"同层可并行"注释不符；max_parallel 双重缺位 | runner.rs:196-235 |
| P2-13 | **external_api 节点半成品**：api_key 读了不用、api_type 忽略、桥接层直接拒绝 | executor.rs:1414、pipeline_bridge.rs:240 |
| P2-14 | **CPU refresh 空实现**（利用率永远 None） | compute/cpu.rs:85 |
| P2-15 | **设置页需重启项无重启入口**；`ui.*`/`disabled_backends` 无编辑 UI | settings.tsx |
| P2-16 | **示例管线模板引用不存在的模块**（funasr-paraformer/qwen2-7b-instruct） | pipeline.tsx:202,218 |
| P2-17 | **packaging 不一致**：entrypoint.service ExecStart 指向 `/usr/bin/entrypoint-daemon`，PKGBUILD 生成的是 `/usr/bin/ep-daemon`；entrypoint.install 文案仍是旧布局 | packaging/ |
| P2-18 | **依赖哈希只覆盖 requirements.txt**：未来引入全局 constraints 时变更不触发重装 | env.rs:537 |

## 4. 死代码清单（💀）

| 项 | 位置 | 说明 |
|---|---|---|
| `ModuleProcess` trait | types.rs:250-256 | 零实现者 |
| `ModuleLifecycle`（485 行编排） | module/lifecycle.rs | 仅测试使用，daemon/desktop 各自手拼 |
| `ComputeScheduler` + status_report | compute/scheduler.rs | 仅测试使用 |
| `state.runner` 全套接线 | ep-daemon state.rs:99-134 | 永不执行，进度回调 pipeline_id 恒空 |
| `EnvManager::set_network` / `ProcessManager::with_network_env` | env.rs / process.rs | 零调用方 |
| `SystemDep::CudaToolkit` 检测 | deps_install.rs:271 | 已定义无触发 |
| `cleanup_hf_cache`（pub） | model.rs:1478 | 零生产调用（可作整合包清理钩子） |
| 前端死件 | placeholder.tsx、status-badge.tsx、client.ts health()/uploadModel()、empty-state 预设、app-store 多数字段 | 无引用 |
| port.rs `is_available`/`allocated_count` | port.rs | 仅测试用 |

## 5. 管线断裂点专题（用户报告症状归因）

纯内置节点链（file_input→file_output、按内置 TOML 执行）**实际是通的**；用户自然路径——拖模块节点、配 ffmpeg——分别命中 P0-1 与 P0-2。桌面端管线页是纯只读查看器（无添加/连线/保存/执行，保存按钮文案"缺少 toml 序列化依赖"已过时——ep-core 的 TOML 序列化早有测试）。

**修复顺序建议**：capability 数据驱动化（/api/modules 暴露 manifest capabilities）→ ffmpeg 契约修正 → 示例模板修正 → 参数 schema 对齐。

## 6. 正面确认（设计时可直接依赖）

- 上传防 zip-slip 设施完备（sanitize_relative_path/resolve_within/symlink 逃逸防护），可提取为整合包共用
- 模型 Ready 判定只看目录非空、meta.source 是自由字符串 → 整合包"pack"来源零障碍接入
- 下载进度 broadcast 链路端到端真实（可复用于整合包导入进度）
- i18n 键集门禁（i18n.rs:176）自动覆盖新命名空间
- 管线 spec 往返一致性有测试保障；PUT /api/pipelines 持久化完整
- IP 过滤中间件真实生效且每请求热读 allow_public
