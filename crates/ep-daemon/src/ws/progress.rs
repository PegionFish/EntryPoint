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

use crate::state::{AppState, ProgressMessage};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/ws/progress", get(ws_progress_handler))
}

async fn ws_progress_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_progress_socket(socket, state))
}

async fn handle_progress_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx: broadcast::Receiver<ProgressMessage> = state.progress_tx.subscribe();
    debug!("WebSocket /ws/progress connected");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(progress_msg) => {
                        let payload = match serde_json::to_string(&progress_msg) {
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
                        tracing::warn!("ws/progress: lagged {n} messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket /ws/progress disconnected");
}
