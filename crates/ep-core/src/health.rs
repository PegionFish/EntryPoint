//! 健康检查模块 — Wave 1a Agent A 实现
//!
//! HTTP 健康检查：轮询模块的 /health 端点直到成功或超时。

use std::time::Duration;

/// 健康检查结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy(String),
    Timeout,
}

/// 对指定端口执行 HTTP 健康检查
pub async fn check_health(
    _port: u16,
    _endpoint: &str,
    _timeout: Duration,
) -> HealthStatus {
    // TODO: Wave 1a Agent A — implement with reqwest
    HealthStatus::Healthy
}
