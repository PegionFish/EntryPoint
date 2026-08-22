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
///
/// 一个条目 = 一台**物理设备**：`id`/`backend`/指标取组内主成员（最高优先级
/// 后端），`stacks` 列出该物理卡被哪些计算栈覆盖（如 7900 XTX →
/// `["rocm","vulkan"]`）。跨栈归并逻辑见 `ep_core::compute::physical`。
#[derive(Debug, Serialize)]
pub(crate) struct DeviceResponse {
    id: String,
    backend: String,
    name: String,
    stacks: Vec<String>,
    total_memory_mb: Option<u32>,
    used_memory_mb: Option<u32>,
    utilization: Option<u8>,
    temperature: Option<u8>,
}

impl DeviceResponse {
    /// 单设备视图（无跨栈别名时退化为自身单栈）
    fn from_device(d: &ComputeDevice) -> Self {
        Self {
            id: d.id.to_string(),
            backend: d.backend.to_string(),
            name: d.name.clone(),
            stacks: vec![d.backend.to_string()],
            total_memory_mb: d.total_memory_mb,
            used_memory_mb: d.used_memory_mb,
            utilization: d.utilization,
            temperature: d.temperature,
        }
    }

    /// 物理归并视图：身份/指标取主成员，名字取最具描述性成员，栈列表为全成员并集
    fn from_group(all: &[ComputeDevice], group: &ep_core::compute::physical::PhysicalGroup) -> Self {
        let primary = &all[group.primary];
        let mut resp = Self::from_device(primary);
        resp.name = ep_core::compute::physical::display_name(all, group).to_string();
        resp.stacks = group
            .members
            .iter()
            .map(|&m| all[m].backend.to_string())
            .collect();
        resp
    }
}

pub async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<DeviceResponse>> {
    let devices = state.devices.read().await;
    // 显示层物理归并：同一物理适配器的多栈视图折叠为单条目；
    // 调度器消费的 state.devices 保持逐栈条目不变
    let groups = ep_core::compute::physical::group_physical_devices(&devices);
    let resp: Vec<DeviceResponse> = groups
        .iter()
        .map(|g| DeviceResponse::from_group(&devices, g))
        .collect();
    Json(resp)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn devices_state(devices: Vec<ComputeDevice>) -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-devices-test-{}-{seq}",
            std::process::id()
        ));
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            devices,
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    async fn get_devices(state: Arc<AppState>) -> Value {
        let app = super::router().with_state(state);
        let req = Request::builder().uri("/devices").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // 字段映射：id/backend 的规范串形式（cuda:0 / cuda / cpu）与
    // 显存/利用率/温度透传；未探测到的指标序列化为 null（前端契约）。
    #[tokio::test]
    async fn list_devices_maps_ids_backends_and_metrics() {
        let devices = vec![
            ComputeDevice {
                id: DeviceId::Cuda(0),
                backend: ComputeBackend::Cuda,
                name: "Test GPU".into(),
                total_memory_mb: Some(24576),
                used_memory_mb: Some(1024),
                utilization: Some(42),
                temperature: Some(55),
            },
            ComputeDevice {
                id: DeviceId::Cpu,
                backend: ComputeBackend::Cpu,
                name: "Test CPU".into(),
                total_memory_mb: Some(16384),
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
        ];
        let body = get_devices(devices_state(devices)).await;
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["id"], "cuda:0");
        assert_eq!(arr[0]["backend"], "cuda");
        assert_eq!(arr[0]["name"], "Test GPU");
        assert_eq!(arr[0]["stacks"], serde_json::json!(["cuda"]));
        assert_eq!(arr[0]["total_memory_mb"], 24576);
        assert_eq!(arr[0]["used_memory_mb"], 1024);
        assert_eq!(arr[0]["utilization"], 42);
        assert_eq!(arr[0]["temperature"], 55);

        assert_eq!(arr[1]["id"], "cpu");
        assert_eq!(arr[1]["backend"], "cpu");
        assert_eq!(arr[1]["total_memory_mb"], 16384);
        assert!(arr[1]["used_memory_mb"].is_null());
        assert!(arr[1]["utilization"].is_null());
        assert!(arr[1]["temperature"].is_null());
    }

    // 无设备状态 → 空数组（新装机/探测失败场景前端不崩）
    #[tokio::test]
    async fn list_devices_empty_returns_empty_array() {
        let body = get_devices(devices_state(vec![])).await;
        assert_eq!(body, serde_json::json!([]));
    }

    // 物理归并：同一物理卡的多栈视图（rocm + vulkan）折叠为单条目，
    // 主身份取最高优先级后端，stacks 列全量覆盖栈，名字取最具描述性者。
    #[tokio::test]
    async fn list_devices_merges_cross_stack_views_of_one_gpu() {
        let devices = vec![
            ComputeDevice {
                id: DeviceId::Rocm(0),
                backend: ComputeBackend::Rocm,
                name: "AMD GPU 0".into(), // rocm-smi 兜底名（旧版键名缺失）
                total_memory_mb: Some(24563),
                used_memory_mb: Some(536),
                utilization: Some(23),
                temperature: Some(41),
            },
            ComputeDevice {
                id: DeviceId::Vulkan(2),
                backend: ComputeBackend::Vulkan,
                name: "AMD Radeon RX 7900 XTX (RADV NAVI31)".into(),
                total_memory_mb: None,
                used_memory_mb: None,
                utilization: None,
                temperature: None,
            },
        ];
        let body = get_devices(devices_state(devices)).await;
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 1, "两条目归并为一个物理设备");
        assert_eq!(arr[0]["id"], "rocm:0", "主成员 = 最高优先级后端");
        assert_eq!(arr[0]["backend"], "rocm");
        assert_eq!(arr[0]["stacks"], serde_json::json!(["rocm", "vulkan"]));
        assert_eq!(
            arr[0]["name"],
            "AMD Radeon RX 7900 XTX (RADV NAVI31)",
            "展示名修复：不再显示通用名 AMD GPU 0"
        );
        // 指标仍来自带数据源的主成员（rocm-smi）
        assert_eq!(arr[0]["total_memory_mb"], 24563);
        assert_eq!(arr[0]["utilization"], 23);
    }
}
