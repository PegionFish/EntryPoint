//! DAG 管线引擎核心
//!
//! 提供管线定义（DAG）、验证、拓扑排序和执行状态机。

pub mod builtin_templates; // T-13：内置 ffmpeg 节点模板库（数据驱动模板表）
pub mod dag;
pub mod executor;
pub mod runner;
pub mod vram; // Wave 2 B3：VRAM 预算（§6.3）

pub use builtin_templates::{BuiltinTemplate, FfmpegBuild, TemplateParam, TemplateParamKind};
pub use dag::{Edge, NodeKind, Pipeline, PipelineNode, PortType, ValidationError};
pub use executor::{ModuleCallError, NodeState, PipelineTask};
pub use runner::{NodeDetail, PipelineRunnerImpl, TaskDetail, TaskSummary};

use std::path::Path;

/// 便捷函数：从 TOML 文件加载管线定义
pub fn load_pipeline(path: &Path) -> anyhow::Result<Pipeline> {
    Pipeline::from_toml(path)
}
