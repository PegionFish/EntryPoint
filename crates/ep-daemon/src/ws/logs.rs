use std::sync::Arc;

use axum::{
    Router,
    extract::{
        State,
        ws::{WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use tokio::sync::broadcast;
use tracing::debug;

use crate::state::{AppState, LogMessage};

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
    debug!("WebSocket /ws/logs connected");

    loop {
        tokio::select! {
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
                // Client sent a message (or disconnected)
                match msg {
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket /ws/logs disconnected");
}
