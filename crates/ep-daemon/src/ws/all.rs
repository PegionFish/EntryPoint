//! 聚合 WebSocket 端点 GET /ws — 统一消息协议
//!
//! 同时订阅 log_tx、progress_tx、model_download_tx 三个通道，
//! 将旧消息类型（LogMessage/ProgressMessage）在转发处映射为统一的
//! [`WsMessage`]（带 `type` 字段），供前端按 `msg.type` 过滤。
//!
//! 旧的 /ws/logs 与 /ws/progress 端点保留不变（见 logs.rs / progress.rs）。

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

use crate::state::{AppState, WsMessage};

/// 心跳间隔（秒）：定期发 Ping 探测半开连接。对端无 Pong 响应 → 关闭连接，
/// 防止断网/掉电的僵尸订阅永久占用订阅槽位。
const WS_PING_INTERVAL_SECS: u64 = 30;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ws", get(ws_all_handler))
}

async fn ws_all_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_all_socket(socket, state))
}

/// 序列化并发送一条 WsMessage。返回 false 表示套接字已断开。
async fn send_msg(socket: &mut WebSocket, msg: &WsMessage) -> bool {
    let payload = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(_) => return true, // 序列化失败跳过该条，不断开连接
    };
    socket.send(Message::Text(payload.into())).await.is_ok()
}

async fn handle_all_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut log_rx = state.log_tx.subscribe();
    let mut progress_rx = state.progress_tx.subscribe();
    let mut download_rx = state.model_download_tx.subscribe();
    // 心跳：上次 Ping 尚未收到 Pong → 判定半开，下一轮断开
    let mut pong_pending = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(WS_PING_INTERVAL_SECS));
    heartbeat.tick().await; // 跳过首个立即 tick，从整 30s 起算
    debug!("WebSocket /ws connected");

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
            r = log_rx.recv() => match r {
                Ok(m) => {
                    // 旧 LogMessage → WsMessage::Log
                    let msg = WsMessage::Log {
                        module_id: m.module_id,
                        line: m.line,
                    };
                    if !send_msg(&mut socket, &msg).await {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws: log channel lagged {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            r = progress_rx.recv() => match r {
                Ok(m) => {
                    // 旧 ProgressMessage → WsMessage::Progress（P2-7：携带 task_id）
                    let msg = WsMessage::Progress {
                        pipeline_id: m.pipeline_id,
                        task_id: m.task_id,
                        node_id: m.node_id,
                        status: m.status,
                    };
                    if !send_msg(&mut socket, &msg).await {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws: progress channel lagged {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            r = download_rx.recv() => match r {
                // model_download_tx 本身即 WsMessage（ModelDownload 变体），直接转发
                Ok(msg) => {
                    if !send_msg(&mut socket, &msg).await {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws: model_download channel lagged {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => {
                // 客户端消息（或断开）；Pong 应答心跳
                match msg {
                    Some(Ok(Message::Pong(_))) => pong_pending = false,
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket /ws disconnected");
}
