//! TCP connect 探活（T-33）——daemon 就绪判定。
//!
//! ep-core 现有 `port.rs` 是"为模块**分配**端口"的 bind 探空闲语义，与本处
//! "探测既有服务是否**可连**"语义不同，故按票面实现独立的最小 connect 探测。

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// 回环上 TCP 端口是否可连（daemon 的 HTTP 端口就绪即视为启动完成）。
///
/// `timeout_ms` 为单次 connect 上限；回环连接失败立即返回（ECONNREFUSED
/// 不等待超时）。
pub fn probe_tcp_loopback(port: u16, timeout_ms: u64) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn probe_true_for_listening_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        assert!(probe_tcp_loopback(port, 500));
        // listener 保持到断言结束
        drop(listener);
    }

    #[test]
    fn probe_false_for_closed_port() {
        // 先用临时监听器圈定一个当前空闲端口，随即释放；回环上对未监听端口
        // 的 connect 立即 ECONNREFUSED（TOCTOU 窗口内被抢占的概率可忽略，
        // 与 ep-core port.rs 测试同一接受口径）
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
            l.local_addr().unwrap().port()
        };
        assert!(!probe_tcp_loopback(port, 500));
    }
}
