//! DAG 管线引擎核心
//!
//! 提供管线定义（DAG）、验证、拓扑排序和执行状态机。

pub mod dag;
pub mod executor;
pub mod runner;

pub use dag::{Edge, NodeKind, Pipeline, PipelineNode, ValidationError};
pub use executor::{NodeState, PipelineTask};

use std::path::Path;

/// 便捷函数：从 TOML 文件加载管线定义
pub fn load_pipeline(path: &Path) -> anyhow::Result<Pipeline> {
    Pipeline::from_toml(path)
}
