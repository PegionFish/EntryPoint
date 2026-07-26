//! 管线执行器（高层接口） — Wave 1b Agent C 实现
//!
//! 基于 PipelineTask 状态机，实际执行各类型节点。

use std::path::{Path, PathBuf};

use crate::pipeline::dag::Pipeline;
use crate::pipeline::executor::{NodeState, PipelineTask};
use crate::types::{PipelineRunner, TaskStatus};

/// 管线运行器
pub struct PipelineRunnerImpl {
    task: Option<PipelineTask>,
    work_dir: PathBuf,
}

impl PipelineRunnerImpl {
    pub fn new(work_dir: PathBuf) -> Self {
        Self {
            task: None,
            work_dir,
        }
    }
}

impl PipelineRunner for PipelineRunnerImpl {
    fn execute(
        &mut self,
        _pipeline: &Pipeline,
        _work_dir: &Path,
    ) -> anyhow::Result<()> {
        // TODO: Wave 1b Agent C — implement actual execution
        todo!("PipelineRunnerImpl::execute — implement in Wave 1b")
    }

    fn task_status(&self) -> &TaskStatus {
        static IDLE: TaskStatus = TaskStatus::Pending;
        match &self.task {
            Some(t) => &t.status,
            None => &IDLE,
        }
    }

    fn node_status(&self, node_id: &str) -> Option<&NodeState> {
        self.task.as_ref()?.node_state(node_id)
    }
}
