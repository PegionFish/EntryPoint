pub mod types;

// 共享基础设施：i18n（嵌入式翻译加载器，资源见仓库根 i18n/locales/）
pub mod i18n;

// Wave 1 modules
pub mod module;
pub mod compute;
pub mod config;

// Wave 2 modules
pub mod env;
pub mod process;
pub mod port;
pub mod model;

// Wave 3 modules
pub mod pipeline;

// Wave 2 B3：任务注册表下沉（daemon/桌面共用，P1-4）
pub mod task_registry;

// Wave 0+ modules (skeleton)
pub mod health;
pub mod deps;
pub mod deps_install;

// 管线中间产物暂存（任务级 RAM 盘生命周期管理）
pub mod staging;

// 极简 cron 解析（管线定时执行）
pub mod cron;

// Wave S 骨架（§4.3 全限定模型 ID，签名冻结，实现由 Wave 1 A3 填入）
pub mod model_id;
