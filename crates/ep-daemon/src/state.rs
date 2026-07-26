use std::sync::Arc;
use tokio::sync::RwLock;

/// Daemon application state — holds all ep-core managers.
/// Will be fleshed out in Wave 2 (Agent D2).
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<RwLock<AppStateInner>>,
}

pub struct AppStateInner {
    // Placeholder — will hold ProcessManager, PortManager, etc. in Wave 2
    pub version: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(AppStateInner {
                version: env!("CARGO_PKG_VERSION").to_string(),
            })),
        }
    }
}
