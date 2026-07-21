# Phase 1 执行计划 — 多代理并行开发

> 目标：Cargo workspace + egui 骨架 + module.toml 解析 + 配置系统 + 计算设备检测
> 策略：最大化并行，最小化依赖阻塞

---

## 并行规则

1. **Wave 0（串行，主代理）**：创建 workspace 骨架 + 共享类型定义（所有 agent 的编译基础）
2. **Wave 1（4 个 agent 并行）**：各 agent 负责独立模块，无交叉写入
3. **Wave 2（4 个 agent 并行）**：依赖 Wave 1 的类型，但彼此独立
4. **Wave 3（主代理）**：集成 + 编译验证 + 修复

## 写入隔离规则

| Agent | 写入范围 | 禁止触碰 |
|---|---|---|
| A (module) | `crates/ep-core/src/module/` | 其他 agent 的文件 |
| B (compute) | `crates/ep-core/src/compute/` | 同上 |
| C (config) | `crates/ep-core/src/config.rs` | 同上 |
| D (ui) | `crates/ep-ui/` | ep-core 内部 |
| E (env) | `crates/ep-core/src/env/` | 同上 |
| F (process) | `crates/ep-core/src/process.rs`, `port.rs` | 同上 |
| G (model) | `crates/ep-core/src/model/` | 同上 |
| H (ui-pages) | `crates/ep-ui/src/pages/` | ep-core |

## 共享契约（Wave 0 定义，所有 agent 只读引用）

- `crates/ep-core/src/types.rs` — 公共类型（ComputeBackend, DeviceId, ModuleCategory 等）
- `crates/ep-core/src/lib.rs` — mod 声明
- `Cargo.toml` (workspace) — 依赖版本锁定

---

## Wave 0：骨架（主代理，串行）

- [ ] workspace Cargo.toml
- [ ] ep-core Cargo.toml + lib.rs + types.rs
- [ ] ep-ui Cargo.toml + main.rs（空壳能编译）
- [ ] 确保 `cargo check` 通过

## Wave 1：核心模块（4 agent 并行）

| Agent | 任务 | 产出 |
|---|---|---|
| A | module manifest 解析器 | `module/manifest.rs` + `module/discovery.rs` |
| B | 计算设备检测 | `compute/mod.rs` + `compute/cuda.rs` + `compute/cpu.rs` |
| C | 配置系统 | `config.rs`（app.toml 读写） |
| D | egui 应用骨架 | `ep-ui/src/app.rs` + 导航 + 5 个空页面 |

## Wave 2：功能模块（4 agent 并行）

| Agent | 任务 | 依赖 |
|---|---|---|
| E | 环境管理器（uv/venv） | types.rs, config.rs |
| F | 进程管理器 + 端口管理器 | types.rs, module/ |
| G | 模型下载管理器 | types.rs, config.rs, module/ |
| H | UI 页面填充（dashboard/modules/settings） | ep-core 公共类型 |

## Wave 3：集成（主代理）

- [ ] 全部 mod 接入 lib.rs
- [ ] `cargo check` 全量编译
- [ ] 修复跨模块引用错误
- [ ] 基础冒烟测试
