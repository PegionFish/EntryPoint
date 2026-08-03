pub mod all;
pub mod logs;
pub mod progress;

use std::sync::Arc;

use axum::Router;

use crate::state::AppState;

pub fn ws_router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(all::router())
        .merge(logs::router())
        .merge(progress::router())
}
