//! 模块生命周期管理 — Wave 2 Agent E 实现
//!
//! 编排模块从发现到运行的完整流程。

/// 模块就绪状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleReadiness {
    /// 缺少运行环境（Python/venv）
    MissingEnv,
    /// 缺少模型文件
    MissingModel,
    /// 就绪，可以启动
    Ready,
    /// 正在运行
    Running,
}

/// 模块生命周期管理器
pub struct ModuleLifecycle;

impl ModuleLifecycle {
    pub fn new() -> Self {
        Self
    }

    /// 检查模块就绪状态
    pub fn get_readiness(&self, _module_id: &str) -> ModuleReadiness {
        // TODO: Wave 2 Agent E — integrate EnvManager + ModelManager + ProcessManager
        ModuleReadiness::Ready
    }
}

impl Default for ModuleLifecycle {
    fn default() -> Self {
        Self::new()
    }
}
