//! 健康检查模块 — Wave 1a Agent A 实现
//!
//! HTTP 健康检查：轮询模块的 /health 端点直到成功或超时。

use std::time::Duration;

/// 健康检查结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// 服务健康（HTTP 200）
    Healthy,
    /// 服务不健康（HTTP 非 200 或连接错误）
    Unhealthy(String),
    /// 在超时时间内未收到 200 响应
    Timeout,
}

/// 轮询间隔
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 对指定端口执行 HTTP 健康检查。
///
/// 轮询 `http://localhost:{port}{endpoint}` 直到返回 HTTP 200 或超过 `timeout`。
/// 轮询间隔为 500ms。
pub async fn check_health(
    port: u16,
    endpoint: &str,
    timeout: Duration,
) -> HealthStatus {
    let url = format!("http://localhost:{}{}", port, endpoint);
    // 健康检查永远只打本机地址：显式禁用代理，避免配置的出口代理
    // （HTTP_PROXY 等）拦截 localhost 流量
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build();

    let client = match client {
        Ok(c) => c,
        Err(e) => return HealthStatus::Unhealthy(format!("failed to build HTTP client: {}", e)),
    };

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match client.get(&url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    return HealthStatus::Healthy;
                } else {
                    let status = response.status();
                    // Not yet healthy, but don't count as unhealthy yet — keep polling
                    if tokio::time::Instant::now() >= deadline {
                        return HealthStatus::Unhealthy(format!(
                            "HTTP {} after timeout",
                            status
                        ));
                    }
                }
            }
            Err(e) => {
                // Connection refused or other error — keep polling
                if tokio::time::Instant::now() >= deadline {
                    return HealthStatus::Unhealthy(format!("request failed: {}", e));
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return HealthStatus::Timeout;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_timeout_no_server() {
        // No server running on this port — should timeout or be unhealthy
        let status = check_health(
            59999,
            "/health",
            Duration::from_secs(2),
        )
        .await;

        // Should be either Timeout or Unhealthy (connection refused)
        assert!(
            matches!(status, HealthStatus::Timeout | HealthStatus::Unhealthy(_)),
            "expected Timeout or Unhealthy, got {:?}",
            status
        );
    }

    #[tokio::test]
    async fn test_health_healthy_with_mock_server() {
        // Start a tiny HTTP server that returns 200 on /health
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        // Spawn a server task that handles requests in a loop
        let server_handle = tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Read the request first
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    // Then write the response
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                    let _ = stream.write_all(response).await;
                    let _ = stream.shutdown().await;
                }
            }
        });

        // Give the server a moment to start accepting
        tokio::time::sleep(Duration::from_millis(50)).await;

        let status = check_health(port, "/health", Duration::from_secs(5)).await;
        assert_eq!(status, HealthStatus::Healthy);

        // Cleanup
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_health_unhealthy_on_non_200() {
        // Start a server that returns 503
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            // Keep accepting connections and returning 503
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let response =
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(response).await;
                    let _ = stream.shutdown().await;
                }
            }
        });

        // Give the server a moment to start accepting
        tokio::time::sleep(Duration::from_millis(50)).await;

        let status = check_health(port, "/health", Duration::from_secs(2)).await;
        // Should be Unhealthy since it never returns 200
        assert!(
            matches!(status, HealthStatus::Unhealthy(_)),
            "expected Unhealthy, got {:?}",
            status
        );

        server_handle.abort();
    }
}
