pub mod health;

use axum::Router;

pub fn api_router() -> Router {
    Router::new()
        .nest("/api", health::router())
}
