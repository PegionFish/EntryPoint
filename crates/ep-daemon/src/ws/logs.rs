use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use tokio::sync::broadcast;
use tracing::debug;

use crate::state::{AppState, LogMessage};

/// 心跳间隔（秒）：定期发 Ping 探测半开连接。对端无 Pong 响应 → 关闭连接，
/// 防止断网/掉电的僵尸订阅永久占用订阅槽位。
const WS_PING_INTERVAL_SECS: u64 = 30;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ws/logs", get(ws_logs_handler))
}

async fn ws_logs_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_logs_socket(socket, state))
}

async fn handle_logs_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx: broadcast::Receiver<LogMessage> = state.log_tx.subscribe();
    // 心跳：上次 Ping 尚未收到 Pong → 判定半开，下一轮断开
    let mut pong_pending = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(WS_PING_INTERVAL_SECS));
    heartbeat.tick().await; // 跳过首个立即 tick，从整 30s 起算
    debug!("WebSocket /ws/logs connected");

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                // 上一轮 Ping 无 Pong → 半开连接（对端断网/掉电），关闭释放订阅
                if pong_pending {
                    break;
                }
                if socket.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
                pong_pending = true;
            }
            msg = rx.recv() => {
                match msg {
                    Ok(log_msg) => {
                        let payload = match serde_json::to_string(&log_msg) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        if socket
                            .send(axum::extract::ws::Message::Text(payload.into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("ws/logs: lagged {n} messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                // Client sent a message (or disconnected); Pong 应答心跳
                match msg {
                    Some(Ok(Message::Pong(_))) => pong_pending = false,
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket /ws/logs disconnected");
}
