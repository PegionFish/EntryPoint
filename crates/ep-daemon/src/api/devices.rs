use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    routing::get,
    Json,
};
use serde::Serialize;

use ep_core::types::ComputeDevice;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/devices", get(list_devices))
}

/// Serializable view of a compute device returned by the API.
#[derive(Debug, Serialize)]
pub(crate) struct DeviceResponse {
    id: String,
    backend: String,
    name: String,
    total_memory_mb: Option<u32>,
    used_memory_mb: Option<u32>,
    utilization: Option<u8>,
    temperature: Option<u8>,
}

impl From<&ComputeDevice> for DeviceResponse {
    fn from(d: &ComputeDevice) -> Self {
        Self {
            id: d.id.to_string(),
            backend: d.backend.to_string(),
            name: d.name.clone(),
            total_memory_mb: d.total_memory_mb,
            used_memory_mb: d.used_memory_mb,
            utilization: d.utilization,
            temperature: d.temperature,
        }
    }
}

pub async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<DeviceResponse>> {
    let devices = state.devices.read().await;
    let resp: Vec<DeviceResponse> = devices.iter().map(DeviceResponse::from).collect();
    Json(resp)
}
