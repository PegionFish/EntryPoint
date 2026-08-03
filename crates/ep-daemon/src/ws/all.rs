//! 聚合 WebSocket 端点 GET /ws — 统一消息协议
//!
//! 同时订阅 log_tx、progress_tx、model_download_tx 三个通道，
//! 将旧消息类型（LogMessage/ProgressMessage）在转发处映射为统一的
//! [`WsMessage`]（带 `type` 字段），供前端按 `msg.type` 过滤。
//!
//! 旧的 /ws/logs 与 /ws/progress 端点保留不变（见 logs.rs / progress.rs）。

use std::sync::Arc;

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
    debug!("WebSocket /ws connected");

    loop {
        tokio::select! {
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
                    // 旧 ProgressMessage → WsMessage::Progress
                    let msg = WsMessage::Progress {
                        pipeline_id: m.pipeline_id,
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
                // 客户端消息（或断开）
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket /ws disconnected");
}
