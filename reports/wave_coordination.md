# 波次协调记录（编排者仲裁与广播）

> 多代理并行开发的跨代理决策记录。新波次提示词必须携带与本波相关的条目。

## 已决仲裁

| # | 事项 | 裁决 | 相关方 |
|---|---|---|---|
| 1 | S2 越界 ep-desktop main.rs 4 个 background_loop stub 分支 | **追认**（穷尽 match 必需，骨架先行原则；C4 在 TODO(C4) 处填实现） | S2→C4 |
| 2 | §6.2 节点字段 `model` vs ep-core dag.rs 现有 `model_id` | **对外契约（TOML/JSON）按冻结 §6.2 用 `model` + 新增 `device`**；B7 负责 dag.rs serde 对齐（rename 或别名），现有管线无该字段值不受影响 | S2→B7/C2/C3 |
| 3 | 未冻结响应形状（§8 仅冻结端点级） | 按 S2 前端提议形状对齐：`VramBudgetResponse`(B3)、`WsPackImportMessage`(B2)、`ModelVariantResponse.needs_download/needs_restart`(B6)、`PackDetail.adaptation`(B2)、multipart 上传字段名统一 `'file'`(B4/B2) | S2→B2/B3/B4/B6 |
| 4 | B2 需要 ep-daemon → ep-pack 依赖 | 编排者已预接线（commit 1cc0a40），B2 直接使用 | S1→B2 |
| 5 | §8.3 配置字段 | 编排者已预接线 AppConfig（python.uv_cache_dir/constraints、compute.cuda_libs_dir、packs.staging_dir、active_models），commit 1cc0a40；A1 维护 python 段，其余只读消费 | →A1/A6/B1/B2 |
| 6 | constraints.txt 缺位（A1 仲裁） | **接受依赖顺序**：文件由 D4（Wave 4）定稿落盘；此前代码静默跳过不阻塞。哈希口径变更导致的一次性模块依赖重装属 P2-18 设计意图——**D4 迁移说明必须提及** | A1→D4 |
| 7 | merge_partial 未知键策略（A1 仲裁） | 保持"忽略未知键"（与 load 的 serde 行为一致）；**C7 知悉**——如需 PUT 拼写错误报错另加严格模式，本期不做 | A1→C7 |
| 8 | A3 越界 ep-pack/Cargo.toml +ep-core 依赖 | **追认**（§4.3 要求统一消费 ep-core model_id 与 ComputeBackend，不可复制解析逻辑） | A3 |
| 9 | A3 保守加严（变体语法经 pin 解析校验 / pipelines.file 拒反斜杠与绝对路径）+ PackManifest 移除 Default | **追认**，与包安全模型和 module.toml 惯例一致 | A3→A4/B1 |
| 10 | A6 越界 lifecycle.rs 2 行 fixture 机械修复 | **追认**（字段新增后的编译必需修复，无行为变化） | A6 |
| 11 | A6 字段扩展后 ep-daemon 字面量缺字段（规则 6 预期断裂） | **编排者门禁期机械补齐**：`api/upload.rs:385`(ModelMeta)、`api/models.rs:374/:927/:942`——补 `qualified_id: None, tags: vec![], pack_id: None` / `qualified_id: None, vram_estimate_mb: None` | A6→门禁 |
| 12 | A2：daemon ProcessManager 补注入（state.rs） | **B2 负责**：`.with_cuda_libs_dir(process::resolve_cuda_libs_dir(&root, &cfg.compute.cuda_libs_dir))` 连同 `with_network_env(cfg.network)`（P1-8 单点） | A2→B2 |
| 13 | A2：桌面端 ProcessManager 缺 with_network_env | **归 C4**（桌面端 main.rs 所有权），随 P0-4/调度器接线一并补 | A2→C4 |
| 14 | B5 发现：ep-core `process::build_module_env` 残留 find(default)+硬编码 models 路径 | **编排者门禁期修复**（ep-core process.rs 本波无主；给 build_module_env 注入 config/激活变体参数，修 --run-module 与桌面路径的变体选择） | B5→门禁 |
| 15 | B1 适配条目形状 vs S2 PackAdaptationEntry | **B2 API 层映射**：`ok = verdict != unsupported`、device 直传、note = i18n(packs:adaptDevice/adaptCpuFallback/adaptUnsupported) | B1→B2 |
| 16 | B1 InstalledPack 缺 name/description | **B1 追加字段返工中**（编排者裁：注册表是唯一持久数据源，Option 字段向后兼容），B2 按字段存在编码、缺失回退 id | B1/B2 |
| 17 | B1 重复导入语义 | **追认硬失败 PackAlreadyInstalled**（先卸载再导入，与"绝不合并"冻结规则自洽） | B1 |
| 18 | B7：runner.rs 层 ffmpeg/任务级 wall-clock 超时+取消（P0-6）归属 | **归 B3**（与 task_registry/超时治理同源）；B7 节点级 HTTP 超时已就位，runner 用 tokio::time::timeout 包裹 execute_node；保留 ModuleCallError downcast | B7→B3 |
| 19 | B7 越界 api/execute.rs 两处测试字面量机械补齐 | **追认**（仲裁 #11 同款） | B7 |
| 20 | B7 schema 变更后 ep-desktop pipeline_editor.rs 预期断裂 | **编排者门禁期机械修复**：:663-679 `NodeKind::Module` 解构补 `device`（或 `..`）；:682-684 `NodeKind::ExternalApi` 去掉已删除的 `api_type`（详情文案改 "llm: {endpoint}"） | B7→门禁 |
| 21 | B2 仲裁：reference 下载 meta 缺 pack_id | **B2 返工中**：packs.rs 内下载完成后补丁 meta（pack_id/qualified_id/tags），不动 ep-core | B2 |
| 22 | B2 仲裁：build 圈选 qualified_id 仅包导入模型有值 | **接受现状**：tags 圈选为通用面（§4.5 tag 组装闭环）；下载/上传路径写 qualified_id 列后续改进项 | B2 |
| 23 | B2 WS 通道选择 | **追认**：pack_import 走 model_download_tx（通用 WsMessage 通道），progress_tx 类型不符 | B2 |
| 24 | B2 build 请求扩展字段 id/name/version/description | **追认**（可选，缺省自动生成身份）；C1 构建向导可直接使用 | B2→C1 |
| 25 | B3/B4 autostart 双实现归一 | **门禁期处理**：B4 `api/autostart.rs` 为权威实现（含失败清理），B3 execution.rs 内同名实现改为委托调用（保留函数壳避免改调用点） | B3/B4→门禁 |
| 26 | B3 越界机械改动（state.rs task_id/bind_persistence、ws/all.rs、execute.rs match 臂、Cargo.toml reqwest） | **追认**（均为编译必需最小改动；B2 合并 state.rs 时注意与 B3 两处共存） | B3 |
| 27 | B3：dag validate 补漏（孤儿/重复边/端口类型）+ PipelineMeta.max_instances + bridge spec_to_toml 保留 max_instances | **B7 返工**（dag.rs/bridge 所有权）；B3 现走 TOML 原文扫描兜底，B7 落地后可切换 | B3→B7 |
| 28 | B3 vram-budget 形状 `device_id`/`items[]`/`unassigned[]` vs S2 types.ts `device` | **B3 形状为契约**（已冗余输出兼容字段）；**C3 消费 device_id**，types.ts 由 C3 对齐 | B3→C3 |
| 29 | C8 先于 C2 完成，C2 的 27 个 components:pipeline.* 键迟到 | **门禁期复活 C8 批量补落盘**（键已登记 i18n_key_requests.md「待落盘-迟到」段）；C2 报告的废弃旧键暂不清理（无害） | C2→C8 |
| 30 | C2：PipelineNodeSpec 缺节点级 timeout_secs/retry_count | **授权 C3 追加式扩展 types.ts**（本波无主；只增不改既有字段）+ toSpec/fromSpec 读写 | C2→C3 |
| 31 | C8 发现：/api/pipelines/execute 请求体未解析 wait/callback_url（引擎 SubmitOptions 已支持） | **编排者门禁期接线**（pipelines.rs execute handler 解析两可选字段 → submit_pipeline_full） | C8→门禁 |
| 32 | C1：types.ts 两处小扩展（ModelDownloadState +'queued'、AppConfig.active_models） | **编排者门禁期机械补**（S2 文件本波无主；C1 已本地放宽不受阻） | C1→门禁 |
| 33 | C1：/api/models 未透传 vram_estimate_mb + 无 active_model_id 暴露 | **编排者门禁期补**（models.rs json 输出加 vram_estimate_mb；modules.rs ModuleResponse 加 active_model_id，均机械透传） | C1→门禁 |
| 34 | C1 约 100 个 i18n 键迟到（common.status.queued + models.* ~55 + packs.* ~45） | **门禁期复活 C8 批量落盘**：键文案以 models.tsx/packs.tsx 源码 defaultValue 为准提取（zh 已定稿，en 需 C8 翻译补齐） | C1→C8 |
| 35 | C6：CLI validate（ep-core DAG 全校验）严于 B1 import（TOML 解析级） | **追认"作者工具从严"**：不下沉全校验到 import（保持 B1 语义）；D1 E2E 注意该差异 | C6→D1 |
| 36 | C6：打包脚本纳入 ep-pack CLI 二进制 | **D3 任务追加**：build.ps1/build.sh 构建目标加 ep-pack-cli（bin 名 ep-pack），GUI/server 包 bin\ 附带 | C6→D3 |
| 37 | C7：log_level daemon 接线需改 main.rs（tracing 初始化顺序）+ check_updates 无消费者（P1-10） | **延后为已知限制**（UI 已如实标注文案；接线归后续迭代）；keep_workspace 清理实现归 D2/D3 评估 | C7→后续 |
| 38 | C7：types.ts 补 §8.3 字段（python.uv_cache_dir/constraints、compute.cuda_libs_dir/single_device、packs、active_models）+ PUT 新形状 | **编排者门禁期机械补**（与 #32 同批；C7 本地 AppConfigExt 届时可回收） | C7→门禁 |
| 39 | C4→C5：pages show() 签名需 cmd_tx（ExecutePipeline/CancelTask 接线） | **已转发 C5**：机械加参；冻结入口 ExecutePipeline{pipeline}/CancelTask{task_id}/RefreshPipelineTasks/ExecuteSingle | C4→C5 |
| 40 | C4：TaskSummary 缺 artifacts 字段（任务页产物列表） | **门禁期编排者扩 ep-core TaskSummary**（runner.rs，机械透传 TaskRecord.artifacts）；C5 先做"打开任务目录"兜底 | C4→门禁 |
| 41 | C3：model pin 双形态（裸变体 vs qualified_id@variant） | **裁决：双形态均合法**（后端 vram.rs rsplit('@') 已兼容；PIPELINE_SPEC 补说明归 C8-revive 文档批）；C2 改产完整 pin 不强制 | C3 |
| 42 | C3：PipelineToolbar 缺 executeDisabled props（VRAM 超限真禁用） | **接受 MVP 现状**（handleExecute 拦截+toast+账本红点）；工具栏 props 增强列后续 | C3 |
| 43 | C3 分支已 fast-forward 合入 C2 提交 42d9cb6 | 门禁合并顺序：先 C2 后 C3（或 C3 直接带入 C2），注意勿重复合并 | C2/C3→门禁 |
| 44 | D3：docs/AUTOMATION.md §4 watcher 文件清单链接待补 | **编排者门禁期补**：链接 scripts/examples/watcher-linux.sh、watcher-windows.ps1、README.md | D3→门禁 |
| 45 | D3：cuda-libs 入 gui+server 双包 + P2-17 连带修复（service 目录对齐/install 用户创建/PKGBUILD 挂 install） | **追认**（存在性复制无副作用；packaging 自洽必要） | D3 |
| 46 | D3：scripts/build-desktop.sh 功能已被 build.sh/build.ps1 gui 覆盖 | **保留暂不裁撤**，列 PROGRESS 后续项 | D3 |
| 47 | 用户裁决（IA 终稿）：模型=模块同一概念；单页「模块」；工具栏「导入模块/导出模块」；旧模块页/整合包页直接删除（无重定向）；变体折叠进选择器不独立成模型 | WebUI+桌面两代理按此执行 | 用户 |
| 48 | 缺陷：daemon 保存配置时把相对路径绝对化持久化（cache_dir/workspace_dir 写为 C:\...），迁移部署目录即坏 | **已修复**（ConfigFix `21ab888`：ResolvedPaths 运行期缓存与序列化字段分离，943 测试全绿）；GET /api/config 回原始形态=预期；requires_restart 更保守=接受 | 门禁发现→修复 |
| 49 | 打包报"18 测试失败" | **build.ps1 计数误报**（旧逻辑数含 "failed" 字样的摘要行，全绿也=18）；已修计数+失败明细（TestStab `d167495`）；测试环境隔离经干扰复现证伪端口争用；**最终打包需合并后重跑** | TestStab |
| 50 | IA-WebUI 重构随设计移除的存量 UI 入口：按模型本机上传/本地路径导入/检查更新/删除模型/tag 筛选 chips（后端端点仍在）；导出产物离开页面无重下入口 | **待用户收尾裁决**（恢复入口 or 确认移除）；孤儿 module-card.tsx 门禁期删除 | IA-WebUI→用户 |
| 51 | DaemonWire 后续：settings.tsx `general.checkUpdatesPending` 文案切回 `checkUpdatesDescription`（后端已接线）；WS `model_update` 类型预留（前端消费延后）；自动检查节奏 15s+24h 常量接受；桌面端 check_updates 不受开关影响（延后） | 文案切换归门禁批；其余接受/延后 | DaemonWire |

## 待收集（各波代理报告中来）

- i18n 键需求 → `reports/i18n_key_requests.md`（C8 Wave 3 统一落盘）
- A5 Windows 真机验证清单 → Wave 5 异构硬件验证输入
