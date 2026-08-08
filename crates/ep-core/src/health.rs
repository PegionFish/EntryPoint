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
    // 端点规范化：不以 / 开头时补上，避免拼出畸形 URL（P3 修复）
    let url = format!("http://localhost:{}/{}", port, endpoint.trim_start_matches('/'));
    // 单次请求超时不超过 1s：与 monitor 的 1s 探测预算对齐（P2 修复），
    // 防止客户端 5s 超时吞掉整个探测预算
    let client_timeout = timeout.min(Duration::from_secs(1));
    // 健康检查永远只打本机地址：显式禁用代理，避免配置的出口代理
    // （HTTP_PROXY 等）拦截 localhost 流量
    let client = reqwest::Client::builder()
        .timeout(client_timeout)
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
                    // 非 2xx：读掉 body 使连接可复用，避免连接被半读挂起
                    let _ = response.bytes().await;
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

    // ─── P2 回归：单次请求超时必须 <= 1s（与 monitor 探测预算对齐） ─────────

    #[tokio::test]
    async fn test_health_client_timeout_tracks_probe_budget() {
        // 服务器 accept 但永不回包：探测时长应由客户端 1s 超时决定，
        // 旧实现客户端 5s 超时会吞掉 1s 的整个探测预算
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1024];
                        // 读请求但永不回包
                        let _ = stream.read(&mut buf).await;
                        let _ = stream.shutdown().await;
                    });
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let started = tokio::time::Instant::now();
        let status = check_health(port, "/health", Duration::from_secs(1)).await;
        let elapsed = started.elapsed();

        assert!(
            matches!(status, HealthStatus::Timeout | HealthStatus::Unhealthy(_)),
            "status: {status:?}"
        );
        // 旧实现客户端超时 5s → 返回需 ~5s；新实现客户端超时 1s → ~1s
        assert!(
            elapsed < Duration::from_secs(4),
            "单次探测不应超过 1s，实际 {elapsed:?}"
        );

        server_handle.abort();
    }

    // ─── P3 回归：endpoint 不以 / 开头时 URL 必须规范化 ────────────────────

    #[tokio::test]
    async fn test_health_endpoint_without_leading_slash() {
        // "health"（无前导 /）→ http://localhost:{port}health 是畸形 URL，
        // 旧实现永远探测失败；规范化后应命中 /health
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_handle = tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                    let _ = stream.write_all(response).await;
                    let _ = stream.shutdown().await;
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let status = check_health(port, "health", Duration::from_secs(2)).await;
        assert_eq!(status, HealthStatus::Healthy);

        server_handle.abort();
    }

    // ─── P2 回归：非 2xx 读掉 body，连接可复用 ─────────────────────────────

    #[tokio::test]
    async fn test_health_non_2xx_drains_body_reuses_connection() {
        // 非 2xx 不读 body 时 reqwest 只能断开连接重连；读掉 body 后所有
        // 轮询请求复用同一连接。服务器 keep-alive 返回 503 并统计 accept 次数：
        // 读 body → accept≈1；不读 → accept≈请求数（2s 预算内约 5 次）
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts2 = accepts.clone();

        let server_handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    accepts2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        loop {
                            match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    let resp = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 9\r\n\r\nnot-ready";
                                    let _ = stream.write_all(resp).await;
                                }
                            }
                        }
                    });
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let status = check_health(port, "/health", Duration::from_secs(2)).await;
        assert!(
            matches!(status, HealthStatus::Unhealthy(_) | HealthStatus::Timeout),
            "status: {status:?}"
        );

        server_handle.abort();
        let n = accepts.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            n <= 2,
            "非 2xx body 未读取导致连接不可复用：accept={n}"
        );
    }
}
