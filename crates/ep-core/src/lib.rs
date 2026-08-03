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

// Wave 0+ modules (skeleton)
pub mod health;
pub mod deps;
pub mod deps_install;
