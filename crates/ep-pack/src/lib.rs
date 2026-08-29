//! EntryPoint 模型整合包（Pack）SDK — daemon 与 CLI 共用的库 crate。
//!
//! # 冻结契约
//!
//! - 包格式（`ep-pack.toml` + `.zip` 归档布局）：`docs/PACK_UNIFY_PLAN.md` §4.2
//! - 全限定模型 ID：§4.3（解析/归一实现位于 `ep_core::model_id`，本 crate 统一消费）
//! - 导入流程与安全模型：§4.4；构建/导出：§4.5；平台适配：§4.6
//!
//! # 模块实现状态与所有权
//!
//! - [`manifest`] — Wave 1 **A3 (PackSchema)** ✅：清单类型 + 校验 + 最小 semver
//!   （`manifest::semver`，min_ep_version 门禁用）
//! - [`build`] / [`extract`] / [`checksum`] — Wave 1 **A4 (PackIO)**：
//!   打包/解包/checksum + zip-slip 路径安全
//! - [`import`] — Wave 2 **B1 (PackImport)**：导入编排 + 适配报告
//!
//! # 跨平台纪律（Windows + Linux 双平台硬约束）
//!
//! 包内路径一律经 `Path`/`PathBuf::join` 组装，禁止硬编码任一平台的分隔符；
//! 归档内相对路径在落盘前必须做清洗（防 zip-slip / symlink 逃逸，
//! 参照 ep-daemon `api/upload.rs` 的既有防护模式）。
//! 纯逻辑 crate，不依赖任何平台 API。

pub mod build;
pub mod checksum;
pub mod extract;
pub mod import;
pub mod manifest;
