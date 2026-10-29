//! T-54：`GET /api/openapi.json` — 机读 OpenAPI 3.1 规范导出（Agent 平台链
//! 第一环：路由发现入口；T-55 MCP server 以本契约为输入）。
//!
//! **手写 serde 结构生成，零新依赖**（不引 utoipa/swagger-ui）：文档由
//! [`document`] 在请求时组装（info.version 取 `CARGO_PKG_VERSION`，随
//! crate 版本自动演进），无静态资源文件、无构建期生成步骤。文档按路径
//! 拆为独立组装函数（单块 `json!` 嵌套过深会撞 serde_json 宏默认递归上限）。
//!
//! # 精度定位（刻意的）
//!
//! 「机器可读**路由发现**」而非完整 schema 校验级规范——每个端点提供
//! `summary` / 参数与请求体形状摘要（宽松 schema：字段名 + 类型 + 说明，
//! 不做必填/约束级断言）/ 错误码表（`x-error-codes` 扩展字段：
//! `[{status, code, description}]`，与 INFERENCE_API.md §2 同源）。
//! Agent 据此可发现「有什么端点、怎么调、错了是什么码」；严格类型校验
//! 留给各端点自身的 400 语义（PARAM_INVALID / INPUT_INVALID）。
//!
//! # 覆盖范围
//!
//! v1 门面（`/api/v1/*`，对外稳定契约）主要端点：capabilities / inference
//! 提交（含 T-54 dry-run 与 idempotency_key）/ tasks 列表·详情·取消 /
//! batches·groups 批量接入 / 旧 result 弃用别名。WebUI 内部端点
//! （`/api` 其余）**不在**覆盖范围——其形状属内部契约（docs/AUTOMATION.md §5）。
//!
//! # 漂移防护
//!
//! `tests` 模块对文档做三重对账：① JSON 可解析 + OpenAPI 3.1 结构完整；
//! ② v1 关键路径/方法全覆盖断言（新增 v1 端点漏登记即测试红）；
//! ③ 每个 operation 必有 summary + responses + 非空 x-error-codes +
//! tags 归属（通用扫描，防个别端点烂尾）。
//!
//! 鉴权：本端点位于 `/api` 前缀下，自动落入 auth.rs 全局访问保护域
//! （token 模式下与其他 `/api/*` 端点同规则）；文档自身不描述全局中间件。

use std::sync::Arc;

use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::state::AppState;

/// 路由：`GET /api/openapi.json`（挂 `/api` 前缀下，见 api/mod.rs）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/openapi.json", axum::routing::get(openapi_json))
}

/// GET /api/openapi.json — 机读规范（每次请求现场组装；文档为纯静态数据，
/// 组装成本可忽略，不做缓存）
async fn openapi_json() -> Json<Value> {
    Json(document())
}

// ─── 组装辅助（收敛重复形状；schema 均为宽松摘要级） ────────────────────────

/// v1 稳定错误体引用（`#/components/schemas/Error`：
/// `{"error":{"code","message"}}`，与 inference.rs `v1_error` 同源）
fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": { "$ref": "#/components/schemas/Error" }
            }
        }
    })
}

/// 数据响应（application/json body + 宽松 schema 摘要）
fn json_response(description: &str, schema: Value) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": { "schema": schema }
        }
    })
}

/// 端点错误码表（`x-error-codes` 扩展；`items = (status, code, description)`）
fn error_codes(items: &[(&str, &str, &str)]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|(status, code, desc)| {
                json!({ "status": status, "code": code, "description": desc })
            })
            .collect(),
    )
}

/// v1 端点公共安全声明（token 鉴权：Bearer 或 X-API-Key；未配置 token 时
/// 实际放行——此处声明的是"配置后的调用方式"）
fn v1_security() -> Value {
    json!([{ "bearerAuth": [] }, { "apiKeyHeader": [] }])
}

/// 路径参数（宽松摘要：仅名字 + 说明，不做约束断言）
fn path_param(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "description": description,
        "schema": { "type": "string" }
    })
}

// ─── 各路径组装（一个路径一个函数，避免单块 json! 过深） ────────────────────

/// GET /api/v1/capabilities
fn path_capabilities() -> Value {
    json!({
        "get": {
            "tags": ["capabilities"],
            "operationId": "listCapabilities",
            "summary": "能力目录聚合：所有已安装模块的 capability 声明（纯只读，永不调用模块进程）",
            "security": v1_security(),
            "responses": {
                "200": json_response(
                    "能力目录（无模块时为空列表）",
                    json!({
                        "type": "object",
                        "required": ["capabilities"],
                        "properties": {
                            "capabilities": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "module_id": { "type": "string" },
                                        "capability": { "type": "string" },
                                        "description": { "type": "string" },
                                        "input_type": { "type": "string", "description": "file | text | …" },
                                        "output_type": { "type": "string" },
                                        "max_file_size_mb": { "type": "integer", "description": "未声明时缺省不出" },
                                        "params": { "type": "object", "description": "参数 schema（字段名 → {type, default?, enum?}）" }
                                    }
                                }
                            }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
            ]),
        }
    })
}

/// POST /v1/inference 的 JSON 形态请求体 schema（宽松摘要）
fn inference_json_body_schema() -> Value {
    json!({
        "type": "object",
        "description": "JSON 形态（input_text 与 input_path 二选一，互斥）",
        "properties": {
            "input_text": { "type": "string", "description": "纯文本输入（物化为 workspace/uploads 下 .txt）" },
            "input_path": { "type": "string", "description": "仅限 workspace/uploads 前缀的既有文件路径（canonicalize 后校验，防穿越）" },
            "params": { "type": "object", "description": "模块参数（按能力 schema 校验 + 默认值注入）" },
            "wait": { "type": "boolean", "description": "true = 同步阻塞至终态（响应 200）；缺省异步（202）" },
            "priority": { "type": "integer", "description": "调度优先级 0-255，越小越先；缺省 50" },
            "bypass_cache": { "type": "boolean", "description": "true = 本次直跑不读不写结果缓存" },
            "force": { "type": "boolean", "description": "true = 跳过动态 VRAM 准入排队（响应附 forced_overcommit/accepted_risk）" },
            "callback_url": { "type": "string", "description": "终态回调（best-effort POST {task_id,status,artifacts}）" },
            "dry_run": { "type": "boolean", "description": "T-54：true = 校验通过后不入队，返回 would_submit + plan（module/capability/解析后 params/目标 device/预估显存）" },
            "idempotency_key": { "type": "string", "maxLength": 128, "description": "T-54：提交幂等键；同键重复提交返回首个 task_id（响应带 idempotent_replay: true）。进程内侧表，不跨重启" }
        }
    })
}

/// POST /v1/inference 的 multipart 形态请求体 schema（宽松摘要）
fn inference_multipart_body_schema() -> Value {
    json!({
        "type": "object",
        "description": "multipart 形态（file 必需；文本字段上限 1MB）",
        "properties": {
            "file": { "type": "string", "format": "binary", "description": "输入文件（大小受能力声明 max_file_size_mb 限制，未声明 2GB 兜底）" },
            "params": { "type": "string", "description": "JSON 字符串（对象）" },
            "wait": { "type": "string", "description": "\"true\" 同步等待" },
            "priority": { "type": "string", "description": "十进制字符串 0-255" },
            "bypass_cache": { "type": "string", "description": "\"true\" 旁路缓存" },
            "force": { "type": "string", "description": "\"true\" 跳过动态准入" },
            "dry_run": { "type": "string", "description": "\"true\" 预演（文件只暂存即清理，不入 uploads）" },
            "idempotency_key": { "type": "string", "description": "≤128 字符，同键重试返回首个 task_id" }
        }
    })
}

/// POST /v1/inference 的 200 响应（wait=true 同步终态 / dry-run 预演，oneOf）
fn inference_sync_or_dryrun_response() -> Value {
    json_response(
        "wait=true 同步终态（status + output_url）或 dry_run=true 预演计划（would_submit + plan），oneOf 二选一",
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "description": "wait=true 同步终态",
                    "properties": {
                        "task_id": { "type": "string" },
                        "status": { "type": "string", "description": "completed | failed | cancelled" },
                        "output_url": { "type": "string", "description": "相对下载 URL（302 → 文件流），绝不返回服务器绝对路径" },
                        "error": { "type": "string", "description": "仅失败时携带" }
                    }
                },
                {
                    "type": "object",
                    "description": "dry_run=true 预演（非提交，零副作用）",
                    "required": ["would_submit", "dry_run", "plan"],
                    "properties": {
                        "would_submit": { "type": "boolean" },
                        "dry_run": { "type": "boolean" },
                        "plan": {
                            "type": "object",
                            "properties": {
                                "module_id": { "type": "string" },
                                "capability": { "type": "string" },
                                "params": { "type": "object", "description": "校验 + 默认值注入后的最终参数" },
                                "device": { "type": "string", "description": "将使用的设备（如 cpu / cuda:0）" },
                                "device_source": { "type": "string", "description": "auto | explicit" },
                                "model_variant": { "type": "string", "description": "解析后的模型变体（无模型声明时缺省不出）" },
                                "vram_estimate_mb": { "type": "integer", "description": "预估显存（manifest 未声明时缺省不出）" },
                                "ram_estimate_mb": { "type": "integer" },
                                "input": { "type": "object", "description": "{kind: path|text|file, …}" }
                            }
                        }
                    }
                }
            ]
        }),
    )
}

/// POST /api/v1/inference/{module_id}/{capability}
fn path_inference_submit() -> Value {
    json!({
        "post": {
            "tags": ["inference"],
            "operationId": "submitInference",
            "summary": "提交推理（multipart 文件或 JSON 两种形态；支持 dry-run 预演与 idempotency_key 幂等重试）",
            "description": "按 Content-Type 分流：multipart/form-data（字段 file 必需 + 可选 params/wait/priority/bypass_cache/force/dry_run/idempotency_key）或 application/json（input_text 与 input_path 二选一）。服务端执行序列：manifest/capability 校验 → 参数 schema 校验与默认值注入 → 输入校验 → 模块自动拉起 → 入队。dry_run=true 时校验通过即返回计划摘要（would_submit: true + plan），零副作用。",
            "security": v1_security(),
            "parameters": [
                path_param("module_id", "模块 id（能力目录 module_id）"),
                path_param("capability", "能力名（能力目录 capability）"),
            ],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": { "schema": inference_json_body_schema() },
                    "multipart/form-data": { "schema": inference_multipart_body_schema() }
                }
            },
            "responses": {
                "202": json_response(
                    "已受理（异步）：task_id（提交时排队另带 queue_position，从 1 起）",
                    json!({
                        "type": "object",
                        "required": ["task_id"],
                        "properties": {
                            "task_id": { "type": "string" },
                            "queue_position": { "type": "integer" },
                            "idempotent_replay": { "type": "boolean", "description": "true = 幂等键命中重放（返回首个 task_id，未创建新任务）" }
                        }
                    }),
                ),
                "200": inference_sync_or_dryrun_response(),
                "400": error_response("PARAM_INVALID / INPUT_INVALID（参数、输入形态、idempotency_key 超长等）"),
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("MODULE_NOT_FOUND / CAPABILITY_NOT_FOUND"),
                "409": error_response("MODEL_NOT_READY（激活变体未就绪）"),
                "429": error_response("QUEUE_FULL（在途任务达容量上限）"),
                "502": error_response("MODULE_START_FAILED（自动拉起失败）"),
            },
            "x-error-codes": error_codes(&[
                ("400", "PARAM_INVALID", "参数缺失/类型不符/枚举越界"),
                ("400", "INPUT_INVALID", "输入缺失、input_path 越权、形态非法、idempotency_key 超 128 字符"),
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "MODULE_NOT_FOUND", "模块未安装/未发现"),
                ("404", "CAPABILITY_NOT_FOUND", "模块无此 capability"),
                ("409", "MODEL_NOT_READY", "激活变体模型未下载就绪"),
                ("429", "QUEUE_FULL", "任务队列已满"),
                ("502", "MODULE_START_FAILED", "模块自动拉起失败"),
                ("500", "INTERNAL", "兜底内部错误"),
            ]),
        }
    })
}

/// GET /api/v1/tasks
fn path_tasks_list() -> Value {
    json!({
        "get": {
            "tags": ["tasks"],
            "operationId": "listTasks",
            "summary": "任务列表（最小稳定面：task_id + status；新任务在前）",
            "security": v1_security(),
            "responses": {
                "200": json_response(
                    "任务列表（详情/产物走 GET /api/v1/tasks/{task_id}）",
                    json!({
                        "type": "object",
                        "required": ["tasks"],
                        "properties": {
                            "tasks": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "task_id": { "type": "string" },
                                        "status": { "type": "string", "description": "queued | running | completed | failed | cancelled" }
                                    }
                                }
                            }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
            ]),
        }
    })
}

/// GET /api/v1/tasks/{task_id}
fn path_task_detail() -> Value {
    json!({
        "get": {
            "tags": ["tasks"],
            "operationId": "getTask",
            "summary": "任务状态与产物查询（轮询用）",
            "security": v1_security(),
            "parameters": [path_param("task_id", "任务 id（提交响应的 task_id）")],
            "responses": {
                "200": json_response(
                    "任务详情（outputs 一律相对下载 URL；queue_position 仅排队时实时携带）",
                    json!({
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "status": { "type": "string", "description": "queued | running | completed | failed | cancelled" },
                            "outputs": { "type": "array", "items": { "type": "object", "properties": {
                                "node_id": { "type": "string" },
                                "url": { "type": "string", "description": "相对下载 URL /api/tasks/{task_id}/artifacts/{node_id}" }
                            } } },
                            "queue_position": { "type": "integer" },
                            "forced_overcommit": { "type": "boolean", "description": "仅 force 提交出现" },
                            "accepted_risk": { "type": "string", "description": "仅 force 提交出现，固定 \"user\"" },
                            "error": { "type": "string", "description": "仅失败时携带" }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("TASK_NOT_FOUND"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "TASK_NOT_FOUND", "任务不存在"),
            ]),
        }
    })
}

/// POST /api/v1/tasks/{task_id}/cancel
fn path_task_cancel() -> Value {
    json!({
        "post": {
            "tags": ["tasks"],
            "operationId": "cancelTask",
            "summary": "取消任务（排队中立即终结不执行；运行中置逻辑终态 cancelled）",
            "security": v1_security(),
            "parameters": [path_param("task_id", "任务 id")],
            "responses": {
                "200": json_response(
                    "已受理取消",
                    json!({
                        "type": "object",
                        "properties": {
                            "task_id": { "type": "string" },
                            "status": { "type": "string" }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("TASK_NOT_FOUND"),
                "409": error_response("TASK_ALREADY_TERMINAL（已终态，不可再取消）"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "TASK_NOT_FOUND", "任务不存在"),
                ("409", "TASK_ALREADY_TERMINAL", "任务已处于终态"),
            ]),
        }
    })
}

/// GET /api/v1/inference/result/{task_id}（弃用别名）
fn path_result_alias() -> Value {
    json!({
        "get": {
            "tags": ["tasks"],
            "operationId": "getInferenceResult",
            "summary": "【弃用别名】任务结果查询（v0.2 起 → GET /api/v1/tasks/{task_id}，形状完全一致）",
            "deprecated": true,
            "security": v1_security(),
            "parameters": [path_param("task_id", "任务 id")],
            "responses": {
                "200": { "$ref": "#/paths/~1api~1v1~1tasks~1{task_id}/get/responses/200" },
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("TASK_NOT_FOUND"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "TASK_NOT_FOUND", "任务不存在"),
            ]),
        }
    })
}

/// POST /api/v1/batches 的请求体 schema（宽松摘要）
fn batch_body_schema() -> Value {
    json!({
        "type": "object",
        "required": ["module_id", "capability", "folder"],
        "properties": {
            "module_id": { "type": "string" },
            "capability": { "type": "string" },
            "folder": { "type": "string", "description": "服务器侧目录（缺省仅 workspace/uploads 内）" },
            "params": { "type": "object", "description": "应用于全部成员文件" },
            "recursive": { "type": "boolean", "description": "缺省仅一层" },
            "extensions": { "type": "array", "items": { "type": "string" }, "description": "大小写不敏感，缺省不过滤" },
            "callback_url": { "type": "string", "description": "透传给每个成员任务" }
        }
    })
}

/// POST /api/v1/batches
fn path_batches() -> Value {
    json!({
        "post": {
            "tags": ["batches"],
            "operationId": "submitBatch",
            "summary": "文件夹批量提交：枚举服务器侧目录内文件逐个提交，聚成任务组（group_id）",
            "description": "folder 缺省仅接受 workspace/uploads 内目录；每个成员文件仍持独立 task_id（轮询/取消走 tasks 端点逐条契约）。",
            "security": v1_security(),
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": { "schema": batch_body_schema() }
                }
            },
            "responses": {
                "202": json_response(
                    "整批受理：group_id + 逐成员 task_ids（单文件提交失败只计入 skipped）",
                    json!({
                        "type": "object",
                        "properties": {
                            "group_id": { "type": "string" },
                            "submitted": { "type": "integer" },
                            "skipped": { "type": "integer" },
                            "task_ids": { "type": "array", "items": { "type": "string" } },
                            "failed_submissions": { "type": "array", "items": { "type": "string" }, "description": "仅存在失败成员时出现" }
                        }
                    }),
                ),
                "400": error_response("INPUT_INVALID / BATCH_TOO_LARGE（单批超 512 个文件，请拆分目录）"),
                "401": error_response("token 缺失或不匹配"),
                "403": error_response("FOLDER_NOT_ALLOWED（folder 越权）"),
                "404": error_response("MODULE_NOT_FOUND / CAPABILITY_NOT_FOUND"),
                "429": error_response("BATCH_QUEUE_INSUFFICIENT（整批超队列余量，整批拒绝）"),
            },
            "x-error-codes": error_codes(&[
                ("400", "PARAM_INVALID", "params schema 校验失败"),
                ("400", "INPUT_INVALID", "folder 不存在/不可读/无匹配文件"),
                ("400", "BATCH_TOO_LARGE", "单批文件数超上限 512"),
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("403", "FOLDER_NOT_ALLOWED", "folder 不在 workspace/uploads 内且未放开配置"),
                ("404", "MODULE_NOT_FOUND", "模块未安装/未发现"),
                ("404", "CAPABILITY_NOT_FOUND", "模块无此 capability"),
                ("429", "BATCH_QUEUE_INSUFFICIENT", "整批超过队列在途余量"),
            ]),
        }
    })
}

/// GET /api/v1/groups
fn path_groups_list() -> Value {
    json!({
        "get": {
            "tags": ["batches"],
            "operationId": "listGroups",
            "summary": "任务组列表（滚动状态 + 计数）",
            "security": v1_security(),
            "responses": {
                "200": json_response(
                    "组列表（status 含 partial = 终态混杂，逐成员以组详情为准）",
                    json!({
                        "type": "object",
                        "properties": {
                            "groups": { "type": "array", "items": { "type": "object", "properties": {
                                "group_id": { "type": "string" },
                                "module_id": { "type": "string" },
                                "capability": { "type": "string" },
                                "status": { "type": "string", "description": "queued | running | completed | failed | cancelled | partial" },
                                "counts": { "type": "object" }
                            } } }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
            ]),
        }
    })
}

/// GET /api/v1/groups/{group_id}
fn path_group_detail() -> Value {
    json!({
        "get": {
            "tags": ["batches"],
            "operationId": "getGroup",
            "summary": "组详情（成员任务逐条状态——权威明细）",
            "security": v1_security(),
            "parameters": [path_param("group_id", "任务组 id（提交批量的 group_id）")],
            "responses": {
                "200": json_response(
                    "组详情（tasks[] 逐成员 task_id/status）",
                    json!({
                        "type": "object",
                        "properties": {
                            "group_id": { "type": "string" },
                            "module_id": { "type": "string" },
                            "status": { "type": "string" },
                            "counts": { "type": "object" },
                            "tasks": { "type": "array", "items": { "type": "object" } }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("GROUP_NOT_FOUND"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "GROUP_NOT_FOUND", "组不存在"),
            ]),
        }
    })
}

/// POST /api/v1/groups/{group_id}/cancel
fn path_group_cancel() -> Value {
    json!({
        "post": {
            "tags": ["batches"],
            "operationId": "cancelGroup",
            "summary": "组取消（非终态成员逐一受理）",
            "security": v1_security(),
            "parameters": [path_param("group_id", "任务组 id")],
            "responses": {
                "200": json_response(
                    "受理结果计数",
                    json!({
                        "type": "object",
                        "properties": {
                            "cancelled_tasks": { "type": "integer" },
                            "already_terminal": { "type": "integer" }
                        }
                    }),
                ),
                "401": error_response("token 缺失或不匹配"),
                "404": error_response("GROUP_NOT_FOUND"),
            },
            "x-error-codes": error_codes(&[
                ("401", "UNAUTHORIZED", "token 缺失或不匹配"),
                ("404", "GROUP_NOT_FOUND", "组不存在"),
            ]),
        }
    })
}

/// 完整 OpenAPI 3.1 文档（`pub(crate)`：tests 模块直接消费做对账断言）。
/// paths 由各路径组装函数逐条登记（新增 v1 端点时同步扩 tests 对账表）。
pub(crate) fn document() -> Value {
    let mut paths = Map::new();
    paths.insert("/api/v1/capabilities".into(), path_capabilities());
    paths.insert(
        "/api/v1/inference/{module_id}/{capability}".into(),
        path_inference_submit(),
    );
    paths.insert("/api/v1/tasks".into(), path_tasks_list());
    paths.insert("/api/v1/tasks/{task_id}".into(), path_task_detail());
    paths.insert("/api/v1/tasks/{task_id}/cancel".into(), path_task_cancel());
    paths.insert(
        "/api/v1/inference/result/{task_id}".into(),
        path_result_alias(),
    );
    paths.insert("/api/v1/batches".into(), path_batches());
    paths.insert("/api/v1/groups".into(), path_groups_list());
    paths.insert("/api/v1/groups/{group_id}".into(), path_group_detail());
    paths.insert(
        "/api/v1/groups/{group_id}/cancel".into(),
        path_group_cancel(),
    );

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "EntryPoint Daemon API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "EntryPoint 本地 AI 平台 daemon 的对外稳定推理契约 (/api/v1/*，Replicate predictions 异步任务模式：提交 → 轮询 → 相对 URL 取产物)。字段级契约与错误码全表见 docs/INFERENCE_API.md；本规范定位机器可读路由发现，schema 为摘要级（不做约束校验）。WebUI 内部端点（/api 其余）不在覆盖范围。"
        },
        "servers": [ { "url": "/", "description": "daemon 本机/局域网根（缺省 http://127.0.0.1:9800）" } ],
        "tags": [
            { "name": "capabilities", "description": "能力发现" },
            { "name": "inference", "description": "推理提交（单任务）" },
            { "name": "tasks", "description": "任务查询与取消" },
            { "name": "batches", "description": "批量接入（任务组）" }
        ],
        "paths": Value::Object(paths),
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer", "description": "Authorization: Bearer <token>（config [api].token 或令牌模式回退 [server].token）" },
                "apiKeyHeader": { "type": "apiKey", "in": "header", "name": "X-API-Key", "description": "与 Bearer 等效的第二凭据通道" }
            },
            "schemas": {
                "Error": {
                    "type": "object",
                    "description": "v1 稳定错误体（不含 i18n 文案；code 全表见 docs/INFERENCE_API.md §2）",
                    "properties": {
                        "error": {
                            "type": "object",
                            "required": ["code", "message"],
                            "properties": {
                                "code": { "type": "string", "description": "稳定机读码（UNAUTHORIZED / PARAM_INVALID / TASK_NOT_FOUND / …）" },
                                "message": { "type": "string" }
                            }
                        }
                    }
                }
            }
        }
    })
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// v1 门面关键路径 → 方法 的对账表（T-54 验收：v1 关键路径全在）。
    /// 新增 v1 端点时同步登记于此（漏登记 = 本测试红，防手写规范腐化）。
    const EXPECTED_V1_ROUTES: &[(&str, &str)] = &[
        ("/api/v1/capabilities", "get"),
        ("/api/v1/inference/{module_id}/{capability}", "post"),
        ("/api/v1/tasks", "get"),
        ("/api/v1/tasks/{task_id}", "get"),
        ("/api/v1/tasks/{task_id}/cancel", "post"),
        ("/api/v1/inference/result/{task_id}", "get"),
        ("/api/v1/batches", "post"),
        ("/api/v1/groups", "get"),
        ("/api/v1/groups/{group_id}", "get"),
        ("/api/v1/groups/{group_id}/cancel", "post"),
    ];

    // ① 文档 JSON 可解析 + OpenAPI 3.1 结构完整（serde_json round-trip）
    #[test]
    fn document_is_valid_json_with_openapi_structure() {
        let doc = document();
        // 可解析性：Value → 字节 → Value round-trip（模拟真实 HTTP 序列化路径）
        let bytes = serde_json::to_vec(&doc).expect("serialize");
        let parsed: Value = serde_json::from_slice(&bytes).expect("文档必须是合法 JSON");
        assert_eq!(parsed["openapi"], "3.1.0");
        assert!(
            parsed["info"]["title"]
                .as_str()
                .unwrap()
                .contains("EntryPoint")
        );
        assert!(!parsed["info"]["version"].as_str().unwrap().is_empty());
        assert!(parsed["paths"].as_object().is_some());
        assert!(parsed["components"]["securitySchemes"].as_object().is_some());
        assert!(parsed["components"]["schemas"]["Error"].is_object());
    }

    // ② v1 关键路径/方法全覆盖对账（漂移测试核心）
    #[test]
    fn v1_key_paths_all_covered_with_methods() {
        let doc = document();
        let paths = doc["paths"].as_object().expect("paths object");
        for (path, method) in EXPECTED_V1_ROUTES {
            let entry = paths
                .get(*path)
                .unwrap_or_else(|| panic!("路径缺失: {path}"));
            assert!(
                entry.get(*method).is_some_and(|op| op.is_object()),
                "路径 {path} 缺少 {method} 操作"
            );
        }
        // 覆盖面不少于对账表（多登记的路径也要结构完整，由 ③ 扫描兜底）
        assert!(paths.len() >= EXPECTED_V1_ROUTES.len());
    }

    // ③ 通用扫描：每个 operation 必有 summary + responses + 非空
    //    x-error-codes + tags 归属（防个别端点烂尾/漏错误码表）
    #[test]
    fn every_operation_has_summary_responses_and_error_codes() {
        let doc = document();
        let paths = doc["paths"].as_object().expect("paths object");
        let http_methods = ["get", "post", "put", "delete", "patch"];
        let mut scanned = 0usize;
        for (path, entry) in paths {
            for method in http_methods {
                let Some(op) = entry.get(method) else {
                    continue;
                };
                assert!(
                    op["summary"].as_str().is_some_and(|s| !s.is_empty()),
                    "{method} {path}: summary 缺失"
                );
                assert!(
                    op["responses"].as_object().is_some_and(|r| !r.is_empty()),
                    "{method} {path}: responses 缺失"
                );
                let codes = op["x-error-codes"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{method} {path}: x-error-codes 缺失"));
                assert!(!codes.is_empty(), "{method} {path}: 错误码表为空");
                for code in codes {
                    assert!(
                        code["status"].is_string(),
                        "{method} {path}: 错误码缺 status"
                    );
                    assert!(code["code"].is_string(), "{method} {path}: 错误码缺 code");
                }
                assert!(
                    op["tags"].as_array().is_some_and(|t| !t.is_empty()),
                    "{method} {path}: tags 缺失"
                );
                scanned += 1;
            }
        }
        assert_eq!(
            scanned,
            EXPECTED_V1_ROUTES.len(),
            "扫描的 operation 数须与对账表一致"
        );
    }

    // T-54 请求面：提交端点的请求 schema 必须声明 dry_run 与 idempotency_key
    //（Agent 发现面——漏声明 = 测试红）
    #[test]
    fn inference_submit_documents_dry_run_and_idempotency_key() {
        let doc = document();
        let op = &doc["paths"]["/api/v1/inference/{module_id}/{capability}"]["post"];
        let props = &op["requestBody"]["content"]["application/json"]["schema"]["properties"];
        assert_eq!(props["dry_run"]["type"], "boolean");
        assert_eq!(props["idempotency_key"]["maxLength"], 128);
        // multipart 形态同样登记（文本字段形态）
        let mp = &op["requestBody"]["content"]["multipart/form-data"]["schema"]["properties"];
        assert!(mp["dry_run"].is_object());
        assert!(mp["idempotency_key"].is_object());
        // dry-run 计划摘要在响应面可见（200 oneOf 的第二形态）
        let schema_200 = &op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(schema_200["oneOf"].as_array().map(Vec::len), Some(2));
        assert!(
            serde_json::to_string(&schema_200)
                .unwrap()
                .contains("would_submit"),
            "200 响应须描述 dry-run 计划形状"
        );
    }

    // 路由接线：GET /api/openapi.json 走完整 api_router 可达（200 + 主要路径存在）
    #[tokio::test]
    async fn openapi_route_serves_document() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-openapi-test-{}", std::process::id())),
            ep_core::config::AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(18700, 18800),
        ));
        let app = crate::api::api_router(state.clone()).with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("响应必须是合法 JSON");
        // 主要路径存在断言（验收：JSON 可解析 + 主要路径存在）
        let paths = body["paths"].as_object().expect("paths object");
        for (path, _) in EXPECTED_V1_ROUTES {
            assert!(paths.contains_key(*path), "openapi.json 缺少路径 {path}");
        }
        assert_eq!(body["openapi"], "3.1.0");
    }
}
