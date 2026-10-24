//! 存储水位端点（T-11）
//!
//! `GET /api/storage` 汇报 workspace / models / tasks 三个目录的已用字节数与
//! 模型缓存盘剩余空间，供前端仪表盘「存储」卡片展示，也用于 NAS 长期运行的
//! 磁盘水位观测。
//!
//! 递归求和在 [`tokio::task::spawn_blocking`] 中执行：目录树可能是深层的 O(n)
//! 扫描，若直接跑在 async worker 上会阻塞轮询请求（T-03 的教训）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use ep_core::model::ModelManager;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/storage", get(storage_report))
}

/// 单个目录的存储水位（响应 `dirs[]` 元素）。
#[derive(Debug, Clone, Serialize)]
pub struct StorageDirReport {
    /// 稳定逻辑名：`workspace` / `models` / `tasks`
    pub name: &'static str,
    /// 解析后的绝对路径（展示用）
    pub path: String,
    /// 目录内所有普通文件的字节数之和（不跟随符号链接）
    pub used_bytes: u64,
}

/// `GET /api/storage` 响应体。
#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    pub dirs: Vec<StorageDirReport>,
    /// 模型缓存盘剩余字节数；探测失败为 `null`（不阻断卡片渲染）
    pub free_bytes: Option<u64>,
}

async fn storage_report(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let (workspace_dir, model_cache_dir) = {
        let config = state.config.read().await;
        (
            config.resolve_workspace_dir(&state.root),
            config.resolve_model_cache_dir(&state.root),
        )
    };
    let tasks_dir = workspace_dir.join("tasks");

    // 三个目录的递归求和 + 模型缓存盘 free 探测，统一丢进阻塞线程池。
    let scan = tokio::task::spawn_blocking(move || {
        let targets: [(&'static str, PathBuf); 3] = [
            ("workspace", workspace_dir),
            ("models", model_cache_dir.clone()),
            ("tasks", tasks_dir),
        ];
        let dirs = targets
            .into_iter()
            .map(|(name, path)| StorageDirReport {
                used_bytes: dir_used_bytes(&path),
                path: path.to_string_lossy().into_owned(),
                name,
            })
            .collect();
        // 复用 ep-core 的跨平台探测（目标盘可用空间，含祖先回退）。
        let free_bytes = ModelManager::available_space_bytes(&model_cache_dir);
        StorageReport { dirs, free_bytes }
    })
    .await;

    let report = match scan {
        Ok(report) => report,
        Err(err) => {
            return super::err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.storage.scanFailed",
                &[("detail", err.to_string())],
            )
            .await;
        }
    };

    (StatusCode::OK, Json(json!(report)))
}

/// 递归统计目录内普通文件字节数。
///
/// - 不跟随符号链接（`file_type().is_symlink()` 直接跳过）：避免重复计数与环路；
/// - 不可读的子目录静默跳过：单点权限问题不应让整张卡片失败；
/// - 目录不存在（如从未导入任务的 tasks/）返回 0。
fn dir_used_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return 0;
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                total = total.saturating_add(len);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use serde_json::Value;

    use crate::state::AppState;

    /// 建一个隔离的 root：workspace/tasks 与 models 下各放一个已知大小文件。
    fn seed_root(root: &Path) -> (u64, u64) {
        let workspace = root.join("workspace");
        let tasks = workspace.join("tasks");
        let models = root.join("models");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::create_dir_all(models.join("large-v3")).unwrap();

        std::fs::write(workspace.join("job.wav"), vec![0u8; 100]).unwrap();
        std::fs::write(tasks.join("task.json"), vec![0u8; 40]).unwrap();
        std::fs::write(models.join("large-v3").join("model.bin"), vec![0u8; 300]).unwrap();

        // workspace 目录总数含 tasks 子目录（100 + 40），models 为 300。
        (140, 300)
    }

    fn test_state(root: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    // 经路由树访问 /api/storage：三个逻辑目录齐全、used 递归求和正确、free 非空。
    #[tokio::test]
    async fn storage_route_reports_three_dirs_used_bytes_and_free_space() {
        let root = std::env::temp_dir().join(format!("ep-api-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (workspace_used, models_used) = seed_root(&root);

        let state = test_state(root.clone());
        let app = super::router().with_state(state);
        let req = Request::builder()
            .uri("/storage")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();

        let dirs = body["dirs"].as_array().expect("dirs array");
        assert_eq!(dirs.len(), 3);
        let names: Vec<&str> = dirs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["workspace", "models", "tasks"]);

        let used = |name: &str| -> u64 {
            dirs.iter()
                .find(|d| d["name"] == name)
                .unwrap_or_else(|| panic!("dir {name}"))
                ["used_bytes"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(used("workspace"), workspace_used);
        assert_eq!(used("models"), models_used);
        assert_eq!(used("tasks"), 40);
        // 路径解析到 root 下（AppConfig 默认相对路径 workspace / models）
        assert!(dirs[0]["path"].as_str().unwrap().contains("workspace"));
        assert!(body["free_bytes"].as_u64().expect("free_bytes") > 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    // 目录不存在（从未导入任务）→ 0，而不是报错。
    #[test]
    fn dir_used_bytes_returns_zero_for_missing_dir() {
        let missing = std::env::temp_dir().join(format!("ep-storage-missing-{}", std::process::id()));
        assert_eq!(super::dir_used_bytes(&missing), 0);
    }

    // 符号链接不跟随：指向大目录的软链不重复计入（Windows 建立软链需权限，unix 限定）。
    #[cfg(unix)]
    #[test]
    fn dir_used_bytes_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("ep-storage-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.bin"), vec![0u8; 64]).unwrap();
        symlink(&real, root.join("link")).unwrap();

        assert_eq!(super::dir_used_bytes(&root), 64);
        let _ = std::fs::remove_dir_all(&root);
    }
}
