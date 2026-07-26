pub mod logs;
pub mod progress;

use axum::Router;

pub fn ws_router() -> Router {
    Router::new()
        .merge(logs::router())
        .merge(progress::router())
}
