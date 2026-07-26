//! 模块系统 — manifest 解析、发现、生命周期

pub mod discovery;
pub mod manifest;
pub mod lifecycle;

pub use discovery::{discover_modules, DiscoveredModule, DiscoveryStatus};
pub use manifest::{
    CapabilityDecl, ComputeConfig, InterfaceConfig, InterfaceType, ModelDecl, ModelSource,
    ModuleError, ModuleInfo, ModuleManifest, ParamSchema, RuntimeConfig, RuntimeType,
};
