//! LLM 提供商档案 API（T-24，Wave2）：`/api/llm/providers` CRUD +
//! 连接测试（daemon 代调 `{base_url}/v1/models`）。
//!
//! 契约（docs/API_DESIGN.md §LLM 提供商）：
//! - `GET /api/llm/providers` — 脱敏视图列表（`api_key` 恒空串 + `api_key_set`）
//! - `POST /api/llm/providers` — 创建（重名 id → 409；id/base_url 校验失败 → 400）
//! - `PUT /api/llm/providers/{provider_id}` — 更新（id 不可改名）：
//!   `api_key` 空串/缺省 = 保留，非空 = 覆盖，显式 `clear_api_key: true` = 清除；
//!   `rpm`/`concurrency`/`proxy` 缺省 = 保留、显式 `null` = 清除
//! - `DELETE /api/llm/providers/{provider_id}` — 删除
//! - `POST /api/llm/providers/{provider_id}/test` — 测试连接：daemon 代调
//!   `GET {base_url}/v1/models`（OpenAI 兼容惯例），超时取 profile
//!   `timeout_secs`；返回成功/失败两态 + 耗时/模型数摘要（恒 200，业务结果在
//!   `ok` 字段，便于前端红绿两态渲染）
//!
//! 安全（P0）：凭据脱敏完全复刻 `api/config.rs` 的 ConfigView 模式——
//! 所有响应（含 400/404/409）带 `Cache-Control: no-store`；`api_key` 恒不回显；
//! PUT 校验失败不落盘、落盘失败回滚内存（`state.config` + `config/app.toml`
//! 一致）。`/api/config` 的 ConfigView 同步脱敏 `llm.providers[].api_key`
//! （见 config.rs）。
//!
//! 实现约束：reqwest 为既有依赖（Wave 2 B3 引入，零新增）；测试连接的请求
//! 构造抽成纯函数（[`models_endpoint_url`] / [`extract_model_count`]）供单测
//! 覆盖，单测不打真实外网。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{get, post, put},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use ep_core::config::LlmProviderConfig;

use super::err_response;
use crate::state::AppState;

/// `timeout_secs` 合法上限（秒）：防误填把连接测试/未来调用挂死一整天
const MAX_TIMEOUT_SECS: u64 = 86_400;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/llm/providers", get(list_providers).post(create_provider))
        .route(
            "/llm/providers/{id}",
            put(update_provider).delete(delete_provider),
        )
        .route("/llm/providers/{id}/test", post(test_provider))
        // 与 /api/config 同口径：凭据相关响应（成功与错误一律）禁止中间层缓存
        .layer(middleware::from_fn(no_store))
}

/// 给本路由组全部响应（含错误）加 `Cache-Control: no-store`
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

// ─── 视图与输入 ─────────────────────────────────────────────────────────────

/// GET/POST/PUT 响应的脱敏视图：`api_key` 恒为空串，是否已配置由
/// `api_key_set` 表达（客户端不得从响应回读密钥明文）。
#[derive(Debug, Serialize)]
pub struct LlmProviderView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// 恒为空串（脱敏）；是否已配置见 [`Self::api_key_set`]
    pub api_key: String,
    /// 服务端是否已配置 api_key
    pub api_key_set: bool,
    pub timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    pub enabled: bool,
}

impl LlmProviderView {
    fn from_config(p: &LlmProviderConfig) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            base_url: p.base_url.clone(),
            api_key: String::new(),
            api_key_set: !p.api_key.trim().is_empty(),
            timeout_secs: p.timeout_secs,
            rpm: p.rpm,
            concurrency: p.concurrency,
            proxy: p.proxy.clone(),
            enabled: p.enabled,
        }
    }
}

/// `Option<Option<T>>` 的字段级反序列化：字段**缺省** → 外层 `None`（不修改），
/// 字段为显式 `null` → `Some(None)`（清除），字段有值 → `Some(Some(v))`（设置）。
///
/// serde 对 `Option<Option<T>>` 的默认行为会把显式 `null` 与字段缺省都折叠成
/// 外层 `None`，无法表达「清除」；本函数（serde_with `double_option` 的手写
/// 等价，避免新增依赖）配合 `#[serde(default, deserialize_with = ...)]`
/// 恢复三态语义。
fn deserialize_double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// POST/PUT /api/llm/providers 请求体。
///
/// 全字段可选：PUT 语义为「缺省 = 保持原值」；POST 语义为「缺省 = 取默认值」。
/// `rpm` / `concurrency` / `proxy` 经 [`deserialize_double_option`] 支持三态；
/// `api_key` 为普通 `Option<String>`：空串/缺省 = 保留（防回传脱敏空串误清），
/// 非空 = 覆盖；清除必须显式 `clear_api_key: true`（优先于 `api_key` 字段）。
/// `id` 仅 POST 有效（PUT 不可改名，路径 id 为准）。
#[derive(Debug, Default, Deserialize)]
pub struct LlmProviderInput {
    pub id: Option<String>,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub timeout_secs: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub rpm: Option<Option<u32>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub concurrency: Option<Option<u32>>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub proxy: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub clear_api_key: Option<bool>,
}

// ─── 纯函数（单测覆盖，不发真实请求）───────────────────────────────────────

/// id 规则：trim 后非空、≤ 128 字节、不含空白与 URL 结构字符（`/` `\` `#` `?`）。
/// id 用作 URL 路径段（`/api/llm/providers/{id}`），故拒绝路径/查询分隔符。
fn validate_provider_id(raw: &str) -> Result<(), ()> {
    if raw.is_empty()
        || raw.len() > 128
        || raw
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '#' | '?'))
    {
        return Err(());
    }
    Ok(())
}

/// base_url 规范化：trim 后必须以 `http://` / `https://` 开头、scheme 后非空、
/// 全程无空白；返回去尾部 `/` 的规范形态。校验与测试连接的 URL 拼接共用。
fn normalize_base_url(raw: &str) -> Result<String, ()> {
    let url = raw.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    match rest {
        Some(rest) if !rest.is_empty() && !url.chars().any(char::is_whitespace) => {
            Ok(url.trim_end_matches('/').to_string())
        }
        _ => Err(()),
    }
}

/// 纯函数：base_url → 测试连接目标 URL `GET {base_url}/v1/models`
/// （OpenAI 兼容提供商惯例；Ollama/vLLM/LM Studio 等本地服务同型）。
fn models_endpoint_url(base_url: &str) -> Result<String, String> {
    normalize_base_url(base_url)
        .map(|base| format!("{base}/v1/models"))
        .map_err(|()| "base_url must be a valid http(s) URL".to_string())
}

/// 纯函数：从 OpenAI 兼容 `GET /v1/models` 响应体提取模型数
/// （`data` 数组长度）；非该形状（含非 JSON）返回 `None`，不视为失败。
fn extract_model_count(body: &Value) -> Option<usize> {
    body.get("data").and_then(Value::as_array).map(Vec::len)
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/llm/providers — 脱敏视图列表
async fn list_providers(State(state): State<Arc<AppState>>) -> Json<Vec<LlmProviderView>> {
    let config = state.config.read().await;
    Json(
        config
            .llm
            .providers
            .iter()
            .map(LlmProviderView::from_config)
            .collect(),
    )
}

/// 校验请求体的 id / base_url / timeout_secs（创建语义：缺省取默认值）。
/// 校验在持锁前完成，失败分支不持有任何锁（err_response 内部要读 config）。
struct ValidatedNew {
    id: String,
    base_url: String,
    timeout_secs: u64,
}

async fn validate_new(
    state: &Arc<AppState>,
    input: &LlmProviderInput,
) -> Result<ValidatedNew, (StatusCode, Json<Value>)> {
    let id = input.id.as_deref().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Err(err_response(
            state,
            StatusCode::BAD_REQUEST,
            "settings.llmApi.idRequired",
            &[],
        )
        .await);
    }
    if validate_provider_id(&id).is_err() {
        return Err(err_response(
            state,
            StatusCode::BAD_REQUEST,
            "settings.llmApi.idInvalid",
            &[],
        )
        .await);
    }
    let base_url = match input.base_url.as_deref().unwrap_or("").trim() {
        "" => {
            return Err(err_response(
                state,
                StatusCode::BAD_REQUEST,
                "settings.llmApi.baseUrlRequired",
                &[],
            )
            .await)
        }
        raw => match normalize_base_url(raw) {
            Ok(u) => u,
            Err(()) => {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "settings.llmApi.baseUrlInvalid",
                    &[],
                )
                .await)
            }
        },
    };
    let timeout_secs = match input.timeout_secs {
        Some(t) if (1..=MAX_TIMEOUT_SECS).contains(&t) => t,
        Some(_) => {
            return Err(err_response(
                state,
                StatusCode::BAD_REQUEST,
                "settings.llmApi.timeoutInvalid",
                &[],
            )
            .await)
        }
        None => LlmProviderConfig::DEFAULT_TIMEOUT_SECS,
    };
    Ok(ValidatedNew {
        id,
        base_url,
        timeout_secs,
    })
}

/// POST /api/llm/providers — 创建（重名 id → 409）。
///
/// 持写锁期间完成重名检查 + 插入 + `save()`；落盘失败回滚内存并返回 500。
async fn create_provider(
    State(state): State<Arc<AppState>>,
    Json(input): Json<LlmProviderInput>,
) -> Result<(StatusCode, Json<LlmProviderView>), (StatusCode, Json<Value>)> {
    let v = validate_new(&state, &input).await?;
    let config_dir = state.root.join("config");

    enum CreateOutcome {
        Created(Box<LlmProviderConfig>),
        IdTaken(String),
        SaveFailed(anyhow::Error),
    }

    let outcome = {
        let mut config = state.config.write().await;
        if config.llm.provider(&v.id).is_some() {
            CreateOutcome::IdTaken(v.id)
        } else {
            let provider = LlmProviderConfig {
                id: v.id,
                name: input.name.as_deref().unwrap_or("").trim().to_string(),
                base_url: v.base_url,
                api_key: input.api_key.as_deref().unwrap_or("").trim().to_string(),
                timeout_secs: v.timeout_secs,
                rpm: input.rpm.unwrap_or(None).filter(|r| *r > 0),
                concurrency: input.concurrency.unwrap_or(None).filter(|c| *c > 0),
                proxy: input
                    .proxy
                    .unwrap_or(None)
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty()),
                enabled: input.enabled.unwrap_or(true),
            };
            let before = config.llm.clone();
            config.llm.providers.push(provider.clone());
            match config.save(&config_dir) {
                Ok(()) => CreateOutcome::Created(Box::new(provider)),
                Err(e) => {
                    config.llm = before;
                    CreateOutcome::SaveFailed(e)
                }
            }
        }
    };

    match outcome {
        CreateOutcome::Created(p) => Ok((StatusCode::CREATED, Json(LlmProviderView::from_config(&p)))),
        CreateOutcome::IdTaken(id) => Err(err_response(
            &state,
            StatusCode::CONFLICT,
            "settings.llmApi.idTaken",
            &[("id", id)],
        )
        .await),
        CreateOutcome::SaveFailed(e) => Err(err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings.llmApi.saveFailed",
            &[("detail", format!("{e:#}"))],
        )
        .await),
    }
}

/// PUT /api/llm/providers/{id} — 更新（id 不可改名）。
///
/// 校验（base_url/timeout 若显式给出）在持锁前完成 → 400 时内存/磁盘零改动；
/// `api_key` 空串/缺省 = 保留、非空 = 覆盖、`clear_api_key: true` = 清除
/// （与 /api/config 的 token 语义同型，防止前端回传脱敏空串误清密钥）。
async fn update_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<LlmProviderInput>,
) -> Result<Json<LlmProviderView>, (StatusCode, Json<Value>)> {
    // 持锁前校验显式给出的 base_url / timeout_secs
    let new_base_url = match input.base_url.as_deref() {
        None => None,
        Some(raw) => match normalize_base_url(raw) {
            Ok(u) => Some(u),
            Err(()) => {
                return Err(err_response(
                    &state,
                    StatusCode::BAD_REQUEST,
                    "settings.llmApi.baseUrlInvalid",
                    &[],
                )
                .await)
            }
        },
    };
    let new_timeout = match input.timeout_secs {
        None => None,
        Some(t) if (1..=MAX_TIMEOUT_SECS).contains(&t) => Some(t),
        Some(_) => {
            return Err(err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "settings.llmApi.timeoutInvalid",
                &[],
            )
            .await)
        }
    };
    let config_dir = state.root.join("config");

    enum UpdateOutcome {
        Updated(Box<LlmProviderConfig>),
        NotFound,
        SaveFailed(anyhow::Error),
    }

    let outcome = {
        let mut config = state.config.write().await;
        if config.llm.provider(&id).is_none() {
            UpdateOutcome::NotFound
        } else {
            let before = config.llm.clone();
            {
                let p = config.llm.provider_mut(&id).expect("existence checked above");
                if let Some(name) = &input.name {
                    p.name = name.trim().to_string();
                }
                if let Some(u) = new_base_url {
                    p.base_url = u;
                }
                if let Some(t) = new_timeout {
                    p.timeout_secs = t;
                }
                if let Some(v) = input.rpm {
                    p.rpm = v.filter(|r| *r > 0);
                }
                if let Some(v) = input.concurrency {
                    p.concurrency = v.filter(|c| *c > 0);
                }
                if let Some(v) = input.proxy {
                    p.proxy = v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                }
                if let Some(enabled) = input.enabled {
                    p.enabled = enabled;
                }
                // api_key：空串/缺省 = 保留（防脱敏回传误清），非空 = 覆盖
                if let Some(key) = input
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                {
                    p.api_key = key.to_string();
                }
                // 显式清除优先于覆盖
                if input.clear_api_key == Some(true) {
                    p.api_key.clear();
                }
            }
            match config.save(&config_dir) {
                Ok(()) => UpdateOutcome::Updated(Box::new(
                    config.llm.provider(&id).expect("still present").clone(),
                )),
                Err(e) => {
                    config.llm = before;
                    UpdateOutcome::SaveFailed(e)
                }
            }
        }
    };

    match outcome {
        UpdateOutcome::Updated(p) => Ok(Json(LlmProviderView::from_config(&p))),
        UpdateOutcome::NotFound => Err(err_response(
            &state,
            StatusCode::NOT_FOUND,
            "settings.llmApi.notFound",
            &[("id", id)],
        )
        .await),
        UpdateOutcome::SaveFailed(e) => Err(err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings.llmApi.saveFailed",
            &[("detail", format!("{e:#}"))],
        )
        .await),
    }
}

/// DELETE /api/llm/providers/{id} — 删除（未知 id → 404）。
///
/// 落盘失败回滚内存并返回 500。
async fn delete_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config_dir = state.root.join("config");

    enum DeleteOutcome {
        Deleted,
        NotFound,
        SaveFailed(anyhow::Error),
    }

    let outcome = {
        let mut config = state.config.write().await;
        match config.llm.providers.iter().position(|p| p.id == id) {
            None => DeleteOutcome::NotFound,
            Some(idx) => {
                let before = config.llm.clone();
                config.llm.providers.remove(idx);
                match config.save(&config_dir) {
                    Ok(()) => DeleteOutcome::Deleted,
                    Err(e) => {
                        config.llm = before;
                        DeleteOutcome::SaveFailed(e)
                    }
                }
            }
        }
    };

    match outcome {
        DeleteOutcome::Deleted => Ok(Json(json!({ "ok": true }))),
        DeleteOutcome::NotFound => Err(err_response(
            &state,
            StatusCode::NOT_FOUND,
            "settings.llmApi.notFound",
            &[("id", id)],
        )
        .await),
        DeleteOutcome::SaveFailed(e) => Err(err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings.llmApi.saveFailed",
            &[("detail", format!("{e:#}"))],
        )
        .await),
    }
}

/// POST /api/llm/providers/{id}/test — 测试连接（daemon 代调）。
///
/// 请求 `GET {base_url}/v1/models`，超时取 profile `timeout_secs`（下限 1s，
/// 防手工改配置写 0 把请求挂死），配置了 `proxy` 时经该代理发出；已配置
/// `api_key` 时附 `Authorization: Bearer <key>`。恒返回 200，业务两态在 `ok`：
/// 成功 `{"ok":true,"elapsed_ms":…,"model_count":…}`；失败
/// `{"ok":false,"elapsed_ms":…,"error":"…"}`（base_url 非法 elapsed_ms = 0）。
async fn test_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 快照 profile（克隆后立即放锁；请求期间不持任何锁）
    let profile = {
        let config = state.config.read().await;
        config.llm.provider(&id).cloned()
    };
    let Some(profile) = profile else {
        return Err(err_response(
            &state,
            StatusCode::NOT_FOUND,
            "settings.llmApi.notFound",
            &[("id", id)],
        )
        .await);
    };

    let url = match models_endpoint_url(&profile.base_url) {
        Ok(url) => url,
        Err(e) => {
            return Ok(Json(json!({
                "ok": false,
                "elapsed_ms": 0,
                "error": e,
            })));
        }
    };

    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(profile.timeout_secs.max(1)));
    if let Some(proxy) = profile.proxy.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        builder = match reqwest::Proxy::all(proxy) {
            Ok(p) => builder.proxy(p),
            Err(e) => {
                return Ok(Json(json!({
                    "ok": false,
                    "elapsed_ms": 0,
                    "error": format!("invalid proxy: {e}"),
                })));
            }
        };
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            return Ok(Json(json!({
                "ok": false,
                "elapsed_ms": 0,
                "error": format!("failed to build http client: {e}"),
            })));
        }
    };

    let mut request = client.get(&url);
    let key = profile.api_key.trim();
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }

    let start = Instant::now();
    let outcome = request.send().await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    match outcome {
        Ok(resp) => {
            let status = resp.status();
            // 响应体可能是任意内容（非 JSON）→ 解析失败只影响 model_count
            let body: Option<Value> = resp.json().await.ok();
            let model_count = body.as_ref().and_then(extract_model_count);
            if status.is_success() {
                Ok(Json(json!({
                    "ok": true,
                    "elapsed_ms": elapsed_ms,
                    "model_count": model_count,
                })))
            } else {
                Ok(Json(json!({
                    "ok": false,
                    "elapsed_ms": elapsed_ms,
                    "error": format!("HTTP {status}"),
                })))
            }
        }
        Err(e) => Ok(Json(json!({
            "ok": false,
            "elapsed_ms": elapsed_ms,
            "error": e.to_string(),
        }))),
    }
}

// ─── Tests（不打真实外网；test 端点仅覆盖纯函数）───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn seq_state() -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-llm-{}-{seq}", std::process::id())),
            ep_core::config::AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    /// 走完整路由（含 no-store 中间件）执行请求，返回状态码 + 响应头 + JSON 体
    async fn route(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let app = router().with_state(state.clone());
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let req = builder
            .body(Body::from(body.unwrap_or("").to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json)
    }

    async fn get_json(state: &Arc<AppState>, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
        route(state, "GET", uri, None).await
    }

    fn cache_control(headers: &axum::http::HeaderMap) -> Option<&str> {
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
    }

    const SECRET_A: &str = "sk-secret-a1";
    const SECRET_B: &str = "sk-secret-b2";

    async fn create_openai(state: &Arc<AppState>) -> (StatusCode, axum::http::HeaderMap, Value) {
        route(
            state,
            "POST",
            "/llm/providers",
            Some(&format!(
                r#"{{"id":"openai","name":"OpenAI 官方","base_url":"https://api.openai.com/","api_key":"{SECRET_A}","timeout_secs":30,"rpm":60,"concurrency":4,"proxy":"http://127.0.0.1:7890","enabled":true}}"#
            )),
        )
        .await
    }

    // ── CRUD 全生命周期（含重名拒绝、脱敏、空串保留、显式清除）───────────

    #[tokio::test]
    async fn crud_full_lifecycle_with_redaction() {
        let state = seq_state();

        // 初始为空
        let (status, headers, json) = get_json(&state, "/llm/providers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache_control(&headers), Some("no-store"), "GET 列表必须 no-store");
        assert_eq!(json, json!([]));

        // 创建：201；响应脱敏（api_key 空串 + api_key_set=true），字段回显
        let (status, headers, json) = create_openai(&state).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(cache_control(&headers), Some("no-store"), "POST 响应必须 no-store");
        assert_eq!(json["id"], "openai");
        assert_eq!(json["api_key"], "", "创建响应 api_key 必须为空串");
        assert_eq!(json["api_key_set"], true);
        assert_eq!(json["base_url"], "https://api.openai.com", "尾斜杠规范化");
        assert_eq!(json["timeout_secs"], 30);
        assert_eq!(json["rpm"], 60);
        assert_eq!(json["concurrency"], 4);
        assert_eq!(json["proxy"], "http://127.0.0.1:7890");
        assert_eq!(json["enabled"], true);

        // 重名 id → 409（且原档案未被破坏）
        let (status, _, json) = route(
            &state,
            "POST",
            "/llm/providers",
            Some(r#"{"id":"openai","base_url":"https://x.example"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json["error"].as_str().is_some_and(|s| !s.is_empty()));

        // 列表：脱敏 + 明文不出现在响应体任何位置
        let (status, headers, json) = get_json(&state, "/llm/providers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache_control(&headers), Some("no-store"));
        assert_eq!(json.as_array().expect("array").len(), 1);
        let body = serde_json::to_string(&json).unwrap();
        assert!(!body.contains(SECRET_A), "明文密钥不得出现在列表响应:\n{body}");

        // 落盘确有明文（设计预期：明文存盘，API 层脱敏）
        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert_eq!(loaded.llm.providers[0].api_key, SECRET_A);

        // PUT 空串 api_key = 保留（脱敏回传不误清）
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(r#"{"name":"OpenAI","timeout_secs":45,"api_key":""}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["api_key_set"], true, "空串 = 保留旧密钥");
        assert_eq!(json["name"], "OpenAI");
        assert_eq!(json["timeout_secs"], 45);
        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert_eq!(loaded.llm.providers[0].api_key, SECRET_A, "旧密钥未被空串覆盖");

        // PUT 非空 api_key = 覆盖
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(&format!(r#"{{"api_key":"{SECRET_B}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["api_key_set"], true);
        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert_eq!(loaded.llm.providers[0].api_key, SECRET_B);

        // PUT clear_api_key = true 显式清除（优先于同时给出的 api_key）
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(&format!(r#"{{"clear_api_key":true,"api_key":"{SECRET_A}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["api_key"], "");
        assert_eq!(json["api_key_set"], false);
        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert!(loaded.llm.providers[0].api_key.is_empty(), "显式清除后落盘为空");

        // PUT 未知 id → 404
        let (status, _, _) = route(
            &state,
            "PUT",
            "/llm/providers/nope",
            Some(r#"{"name":"x"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 删除 → ok；列表回空；重复删除 → 404
        let (status, _, json) = route(&state, "DELETE", "/llm/providers/openai", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["ok"], true);
        let (_, _, json) = get_json(&state, "/llm/providers").await;
        assert_eq!(json.as_array().expect("array").len(), 0);
        let (status, _, _) = route(&state, "DELETE", "/llm/providers/openai", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert!(loaded.llm.providers.is_empty(), "删除须落盘");
    }

    // ── 创建校验：id / base_url / timeout 非法 → 400 且零副作用 ─────────

    #[tokio::test]
    async fn create_validation_rejects_bad_id_base_url_and_timeout() {
        let state = seq_state();

        let cases: Vec<(&str, &str)> = vec![
            ("缺 id", r#"{"base_url":"https://x.example"}"#),
            ("空 id", r#"{"id":"  ","base_url":"https://x.example"}"#),
            ("id 含空白", r#"{"id":"a b","base_url":"https://x.example"}"#),
            ("id 含斜杠", r#"{"id":"a/b","base_url":"https://x.example"}"#),
            ("缺 base_url", r#"{"id":"x"}"#),
            ("空 base_url", r#"{"id":"x","base_url":"  "}"#),
            ("非法 scheme", r#"{"id":"x","base_url":"ftp://x.example"}"#),
            ("scheme 后为空", r#"{"id":"x","base_url":"https://"}"#),
            ("base_url 含空白", r#"{"id":"x","base_url":"https://a b.com"}"#),
            ("timeout=0", r#"{"id":"x","base_url":"https://x.example","timeout_secs":0}"#),
            ("timeout 超上限", r#"{"id":"x","base_url":"https://x.example","timeout_secs":90000}"#),
        ];
        for (tag, body) in cases {
            let (status, _, json) = route(&state, "POST", "/llm/providers", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {tag} 应 400");
            assert!(json["error"].as_str().is_some_and(|s| !s.is_empty()), "case {tag} 应带错误文案");
        }

        // 全部拒绝后零副作用
        let (_, _, json) = get_json(&state, "/llm/providers").await;
        assert_eq!(json.as_array().expect("array").len(), 0, "非法创建不得留档");
    }

    // ── PUT 校验失败：400 + 内存/磁盘回滚 ───────────────────────────────

    #[tokio::test]
    async fn update_invalid_field_rejected_with_memory_unchanged() {
        let state = seq_state();
        let (_, _, _) = create_openai(&state).await;

        // 非法 timeout + 合法 name：all-or-nothing，name 不得部分写入
        let (status, headers, _) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(r#"{"name":"changed","timeout_secs":0}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(cache_control(&headers), Some("no-store"), "错误响应同样 no-store");

        let cfg = state.config.read().await;
        assert_eq!(cfg.llm.providers[0].name, "OpenAI 官方", "400 时内存不得部分写入");
        assert_eq!(cfg.llm.providers[0].timeout_secs, 30);
        assert_eq!(cfg.llm.providers[0].api_key, SECRET_A);
        drop(cfg);

        let loaded = ep_core::config::AppConfig::load(state.root.join("config").as_path())
            .expect("reload config");
        assert_eq!(loaded.llm.providers[0].name, "OpenAI 官方", "400 时磁盘不得变化");
    }

    // ── PUT 三态可选字段：缺省保留 / null 清除 / 值覆盖 ─────────────────

    #[tokio::test]
    async fn update_optional_fields_three_state_semantics() {
        let state = seq_state();
        let (_, _, _) = create_openai(&state).await;

        // 缺省 = 保留
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(r#"{"name":"kept"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["rpm"], 60, "缺省字段保留");
        assert_eq!(json["proxy"], "http://127.0.0.1:7890");

        // 显式 null = 清除（键从响应中消失）
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(r#"{"rpm":null,"concurrency":null,"proxy":null}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.get("rpm").is_none(), "rpm null 清除后键应消失");
        assert!(json.get("concurrency").is_none());
        assert!(json.get("proxy").is_none());

        {
            let cfg = state.config.read().await;
            assert_eq!(cfg.llm.providers[0].rpm, None);
            assert_eq!(cfg.llm.providers[0].concurrency, None);
            assert_eq!(cfg.llm.providers[0].proxy, None);
            assert_eq!(cfg.llm.providers[0].name, "kept", "未给出的字段不受影响");
        }

        // 0 值 = 不限（归一化为 None）
        let (status, _, json) = route(
            &state,
            "PUT",
            "/llm/providers/openai",
            Some(r#"{"rpm":0,"concurrency":0}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.get("rpm").is_none());
        assert!(json.get("concurrency").is_none());
    }

    // ── test-connection 纯函数（不发真实请求）──────────────────────────

    #[test]
    fn models_endpoint_url_normalizes_and_appends_v1_models() {
        assert_eq!(
            models_endpoint_url("https://api.openai.com").unwrap(),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_endpoint_url("https://api.openai.com/").unwrap(),
            "https://api.openai.com/v1/models",
            "尾斜杠去重"
        );
        assert_eq!(
            models_endpoint_url("  http://127.0.0.1:11434/  ").unwrap(),
            "http://127.0.0.1:11434/v1/models",
            "首尾空白容忍"
        );
        assert_eq!(
            models_endpoint_url("https://host/v1").unwrap(),
            "https://host/v1/v1/models",
            "已有 /v1 前缀不做特殊处理（拼接规则恒定）"
        );

        for bad in ["", "   ", "ftp://x", "https://", "http://", "a b", "https://a b"] {
            assert!(models_endpoint_url(bad).is_err(), "非法 base_url 应报错: {bad:?}");
        }
    }

    #[test]
    fn validate_provider_id_rules() {
        assert!(validate_provider_id("openai").is_ok());
        assert!(validate_provider_id("A-z_09.本地").is_ok(), "允许非 ASCII 逻辑名");
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id("a b").is_err());
        assert!(validate_provider_id("a/b").is_err());
        assert!(validate_provider_id("a\\b").is_err());
        assert!(validate_provider_id("a#b").is_err());
        assert!(validate_provider_id("a?b").is_err());
        assert!(validate_provider_id(&"x".repeat(129)).is_err());
        assert!(validate_provider_id(&"x".repeat(128)).is_ok());
    }

    #[test]
    fn extract_model_count_reads_openai_data_array() {
        assert_eq!(
            extract_model_count(&json!({"object":"list","data":[{"id":"a"},{"id":"b"},{"id":"c"}]})),
            Some(3)
        );
        assert_eq!(extract_model_count(&json!({"data":[]})), Some(0));
        assert_eq!(extract_model_count(&json!({})), None, "非 OpenAI 形状不算失败");
        assert_eq!(extract_model_count(&json!({"data": 3})), None);
        assert_eq!(extract_model_count(&json!(null)), None);
    }

    // ── 路由注册守护（401 免验：全局中间件在 main.rs 装配，此处走裸 router）──

    #[tokio::test]
    async fn routes_registered_not_falling_to_api_fallback() {
        let state = seq_state();
        // 各路由命中（业务结果而非 /api fallback 的 i18n 404）：
        // GET 列表 200 / GET 未知 id 无该方法 → 405 亦可证明路由命中
        let (status, _, _) = route(&state, "PATCH", "/llm/providers/openai", Some("{}")).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "PUT/DELETE 已注册的路径 PATCH 应 405 而非 fallback 404");
    }
}
