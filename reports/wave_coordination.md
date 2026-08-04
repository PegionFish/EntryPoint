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

## 待收集（各波代理报告中来）

- i18n 键需求 → `reports/i18n_key_requests.md`（C8 Wave 3 统一落盘）
- A5 Windows 真机验证清单 → Wave 5 异构硬件验证输入
