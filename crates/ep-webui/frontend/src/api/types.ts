// ===== Devices =====
export interface DeviceResponse {
  id: string
  backend: string
  name: string
  total_memory_mb: number | null
  used_memory_mb: number | null
  utilization: number | null
  temperature: number | null
}

// ===== Module capabilities (§8.2 / P0-1：manifest 原样透传) =====

/** 能力参数 schema（§5.3 直跑参数表单数据驱动：type/default/min/max/enum） */
export interface CapabilityParamSchema {
  /** 参数值类型（string/int/float/boolean/enum…，manifest 自由声明） */
  type: string
  default?: unknown
  description?: string | null
  min?: number | null
  max?: number | null
  step?: number | null
  /** 枚举可选值（对应 manifest `enum` 字段） */
  enum?: string[] | null
  options?: string[] | null
}

/** 模块能力声明（ep-core `CapabilityDecl` 序列化原样） */
export interface CapabilityDecl {
  name: string
  description: string
  /** 输入/输出数据类型（audio/video/image/text/json/file） */
  input_type: string
  output_type: string
  max_file_size_mb?: number | null
  supports_batch?: boolean
  /** 参数 schema（键 = 参数名）；直跑表单与管线节点参数面板据此渲染 */
  params?: Record<string, CapabilityParamSchema> | null
}

// ===== Modules =====
export interface ModuleResponse {
  id: string
  name: string
  version: string
  description: string
  category: string
  path: string
  status: string
  service_status: string
  /** 模块 manifest 声明的能力列表（§8.2；后端 B5 上线前的过渡期可能缺失） */
  capabilities?: CapabilityDecl[]
  /** 当前绑定设备（如 "cuda:0"；未运行为 null） */
  device?: string | null
  /**
   * 解析后的激活变体 id（config.active_models → default → 首变体；
   * 无模型模块为 null）——模块卡「变体选择器」选中值的权威数据源
   */
  active_model_id?: string | null
}

export interface ModuleStatusResponse {
  module_id: string
  status: string
  port: number | null
  uptime_secs: number
}

export interface ModuleLogsResponse {
  module_id: string
  lines: string[]
}

export interface ModuleActionResult {
  status?: string
  module_id?: string
  port?: number
  error?: string
}

// ===== Config =====
export interface AppConfig {
  server: { host: string; port: number; allow_public: boolean }
  general: {
    language: string
    theme: string
    log_level: string
    check_updates: boolean
  }
  compute: {
    strategy: string
    disabled_backends: string[]
    refresh_interval_secs: number
    allow_overcommit: boolean
    single_device?: string | null
    /** 共享 CUDA 库目录（§8.3/§3.1，门禁 #38） */
    cuda_libs_dir?: string
  }
  ports: { range_start: number; range_end: number }
  models: {
    cache_dir: string
    hf_endpoint: string
    default_source: string
    max_concurrent_downloads: number
    cache_paths: string[]
  }
  python: {
    path: string
    uv_path: string
    /** uv 缓存目录（§8.3/§3.1，门禁 #38） */
    uv_cache_dir?: string
    /** 全局 constraints 文件（空 = 停用，§3.1） */
    constraints?: string
  }
  pipeline: {
    max_parallel: number
    default_timeout_secs: number
    keep_workspace: boolean
    workspace_dir: string
  }
  network: { http_proxy: string; https_proxy: string; no_proxy: string }
  /** 整合包配置（§8.3，门禁 #38） */
  packs?: { staging_dir?: string }
  ui: { scale_factor: number; font_size: number; dashboard_refresh_secs: number }
  /** 每模块激活模型变体（§5.2 单槽位）：module_id → model_id；后端恒返回，旧前端类型缺失故设可选 */
  active_models?: Record<string, string>
}

// ===== Models =====
/** 模型来源：HuggingFace / ModelScope / 自定义 URL */
export type ModelSource = 'huggingface' | 'modelscope' | 'url'

/** 模型下载状态机（WS 推送与下载列表共用；queued = 并发闸排队，B6/P2-1，门禁 #32） */
export type ModelDownloadState =
  | 'queued'
  | 'downloading'
  | 'completed'
  | 'failed'
  | 'cancelled'

export interface ModelInfo {
  model_id: string
  name: string
  target_dir: string
  status: string
  source: string
  size_estimate_mb: number
  /** 该模型可用的下载来源（后端即将返回，过渡期可能缺失） */
  available_sources?: ModelSource[]
  /** 全限定模型 ID `<publisher>.<vendor>.<model>`（§4.3；过渡期可能缺失） */
  qualified_id?: string
  /** 用户 tag（§5.1 统一页 chips，存 meta，随整合包流转） */
  tags?: string[]
  /** 来源整合包 id（§4.4；source="pack" 时存在） */
  pack_id?: string | null
  /** 变体级 VRAM 估算 MB（§6.3；模块级兜底） */
  vram_estimate_mb?: number | null
}

/** 进行中的模型下载任务（GET /api/models/downloads） */
export interface ModelDownloadStatus {
  module_id: string
  model_id: string
  source: ModelSource
  percent: number
  bytes: number
  state: ModelDownloadState
}

export interface ModelListResponse {
  modules: { module_id: string; module_name: string; models: ModelInfo[] }[]
}

export interface ModelDetail {
  model_id: string
  name: string
  target_dir: string
  status: string
  size_bytes: number | null
  file_count: number | null
  local_cache_path: string | null
}

export interface ModelDetailResponse {
  module_id: string
  module_name: string
  models: ModelDetail[]
}

export interface ImportRequest {
  model_id: string
  source_path: string
}

export interface ImportResponse {
  status?: string
  module_id?: string
  model_id?: string
  target_dir?: string
  file_count?: number
  total_bytes?: number
  error?: string
}

// ===== Dependencies =====
/** 单个模块的 PyTorch CUDA 检测结果（与后端 /api/deps 实际返回对齐） */
export interface TorchCudaStatus {
  module_id: string
  venv_path: string
  torch_version: string | null
  cuda_available: boolean
  guidance: string | null
}

export interface DepReport {
  ffmpeg: {
    available: boolean
    version: string | null
    path: string | null
    guidance: string | null
  }
  torch_cuda: TorchCudaStatus[]
}

// ===== WebSocket =====
/** 全局 /ws 聚合端点推送的实时日志行 */
export interface WsLogMessage {
  type: 'log'
  module_id: string
  line: string
}

/** 管线节点执行进度 */
export interface WsProgressMessage {
  type: 'progress'
  pipeline_id: string
  /** 任务身份（§8.2，修 P2-7 并发串染；后端过渡期可能缺失） */
  task_id?: string
  node_id: string
  status: string
}

/** 模型下载进度 */
export interface WsModelDownloadMessage {
  type: 'model_download'
  module_id: string
  model_id: string
  /** 下载进度百分比（0-100） */
  percent: number
  state: ModelDownloadState
  /** 已下载字节数 */
  bytes: number
}

/** 整合包导入进度（§4.4 / §8.2 新增 WS 消息类型） */
export interface WsPackImportMessage {
  type: 'pack_import'
  pack_id: string
  /** 当前阶段（staging/unpack/checksum/models/pipelines/register…） */
  stage?: string
  /** 百分比 0-100；无法估算进度时可能缺失 */
  percent?: number
  /** 进度态：running/completed/failed */
  state?: string
  /** 阶段说明或错误信息 */
  message?: string
}

/** 后台自动更新检查发现可用更新（P1-10/P2-1：general.check_updates 自动检查广播） */
export interface WsModelUpdateMessage {
  type: 'model_update'
  module_id: string
  model_id: string
  /** 本地化说明（daemon 按服务器语言渲染，含远端更新时间） */
  reason: string
}

/** /ws 聚合消息（按 type 判别） */
export type WsMessage =
  | WsLogMessage
  | WsProgressMessage
  | WsModelDownloadMessage
  | WsPackImportMessage
  | WsModelUpdateMessage

// ===== Tasks =====
export interface TaskSummary {
  id: string
  pipeline_name: string
  status: string
  started_at?: string
  finished_at?: string
  node_count: number
  completed_nodes: number
  /** 所属管线 id（§6.8 任务↔管线身份；历史记录可能缺失） */
  pipeline_id?: string
  /** 队列位置（§6.8；status="queued" 时存在，全局/管线闸门等待） */
  queue_position?: number | null
  /** 实际开始运行时间（§6.8 pipeline_tasks 响应；排队耗时可算） */
  started_running_at?: string
  /** 任务错误信息（failed 时存在） */
  error?: string
}

export interface TaskNodeState {
  node_id: string
  state: string
  error?: string
}

export interface TaskDetail extends TaskSummary {
  nodes: TaskNodeState[]
}

export interface TaskArtifact {
  node_id: string
  name: string
  size: number
}

// ===== Pipelines =====
export interface PipelineSummary {
  id: string
  name: string
  description: string
  source: 'builtin' | 'custom'
}

export interface PipelineNodeSpec {
  id: string
  label: string
  kind: 'builtin' | 'module'
  builtin?: string
  module_id?: string
  capability?: string
  /** 变体 pin `<qualified_id>@<variant>`（§6.2；缺省 = 跟随激活变体，执行前校验 §5.2） */
  model?: string
  /** 设备绑定软约束（§6.2："auto" | "cuda:0" | "rocm:1" | "openvino:GPU.0"…；本机无此设备时警告并回退 auto） */
  device?: string
  params: Record<string, unknown>
  position?: { x: number; y: number }
  /** 节点级超时（秒）— P1-11：后端 SpecNode 透传；前端暂无编辑面，往返保真不丢失 */
  timeout_secs?: number
  /** 节点级重试次数 — P1-11：同 timeout_secs */
  retry_count?: number
}

export interface PipelineEdgeSpec {
  from: [string, string]
  to: [string, string]
}

export interface PipelineSpec {
  pipeline: { id: string; name: string; description: string }
  nodes: PipelineNodeSpec[]
  edges: PipelineEdgeSpec[]
}

export interface ExecutePipelineRequest {
  pipeline_id?: string
  spec?: PipelineSpec
  inputs?: Record<string, Record<string, unknown>>
  /** 同步模式（§6.5）：阻塞至终态，响应直接带 status + artifacts */
  wait?: boolean
  /** 完成回调（§6.5）：终态时 POST {task_id, status, artifacts}，best-effort */
  callback_url?: string
}

export interface ExecutePipelineResponse {
  task_id: string
  /** 同步模式（wait=true）终态快照：任务状态（§6.5；异步模式缺失） */
  status?: string
  /** 同步模式（wait=true）终态快照：产物清单（§6.5） */
  artifacts?: TaskArtifact[]
}

// ===== Packs（整合包 §4 / §8.1）=====

/** 整合包内模型声明（§4.2 [[models]]） */
export interface PackModelRef {
  qualified_id: string
  variant?: string
  /** reference=仅描述符引用（导入时下载） | bundle=权重随包 */
  mode: 'reference' | 'bundle'
  tags?: string[]
}

/** 已安装整合包注册表条目（GET /api/packs 列表项，§4.4 runtime/packs/<id>.json） */
export interface PackInfo {
  /** 全局唯一 `<publisher>.<pack-name>` */
  id: string
  version: string
  name: string
  description?: string
  authors?: string[]
  license?: string
  homepage?: string
  tags?: string[]
  /** 包声明可利用的后端（§4.2 [compute].backends，导入时与本机设备比对） */
  backends?: string[]
  models?: PackModelRef[]
  pipelines?: string[]
  /** 安装时间（ISO-8601） */
  installed_at?: string
}

/** 整合包导入适配报告的逐模型结论（§4.6） */
export interface PackAdaptationEntry {
  qualified_id: string
  variant?: string
  /** 是否可在本机运行 */
  ok: boolean
  /** 结论设备（如 "cuda:0"）；null = CPU 保底或不支持 */
  device?: string | null
  /** 结论文案（"将运行于 cuda:0" / "CPU 保底" / "不支持（原因）"） */
  note?: string
}

/** GET /api/packs/{id} 响应（详情 = 注册条目 + 内容清单/适配报告） */
export interface PackDetail extends PackInfo {
  adaptation?: PackAdaptationEntry[]
}

/** POST /api/packs/import 请求（§8.1：本地路径或 URL；浏览器上传走 uploadPack） */
export type PackImportRequest =
  | { source: 'local'; path: string }
  | { source: 'url'; url: string }

/** 导入/上传 202 受理响应：后续进度走 WS pack_import 消息 */
export interface PackImportResponse {
  pack_id: string
}

/** POST /api/packs/build 请求（§4.5：tag 组装闭环） */
export interface PackBuildRequest {
  /** 圈选模型（`<qualified_id>@<variant>` 列表） */
  models: string[]
  /** 打包携带的管线 id */
  pipelines?: string[]
  /** 以 bundle 模式携带权重的 qualified_id 列表 */
  bundle?: string[]
  /** 按 tag 圈选模型 */
  tags?: string[]
  /** 包身份字段（可选，缺省自动生成）：`<publisher>.<pack-name>` */
  id?: string
  /** 显示名称（可选） */
  name?: string
  /** 版本号（可选） */
  version?: string
  /** 描述（可选） */
  description?: string
}

/** POST /api/packs/build 202 响应：构建完成后经 export 端点下载 .epzip */
export interface PackBuildResponse {
  pack_id: string
}

// ===== 直跑（§5.3 / §8.1）=====

/** POST /api/execute/single 请求（模块未运行时后端自动拉起并等健康） */
export interface DirectExecRequest {
  module_id: string
  /** 能力裸名（来自 manifest capabilities，修 P0-1 命名失配） */
  capability: string
  /** 执行参数（按 CapabilityDecl.params schema 渲染提交） */
  params?: Record<string, unknown>
  /** 服务器本地输入文件路径（浏览器端先经 uploadInput 暂存） */
  input_path: string
}

/** POST /api/execute/single 202 响应 */
export interface DirectExecResponse {
  task_id: string
}

/** POST /api/upload/input 响应（workspace/uploads 暂存路径） */
export interface UploadInputResponse {
  path: string
}

// ===== 管线任务 / VRAM 预算（§6.3 / §6.8 / §8.1）=====

/** GET /api/pipelines/{id}/tasks 查询参数（§6.8 管线级任务视图） */
export interface PipelineTasksQuery {
  /** 按任务状态过滤（含 queued） */
  status?: string
  limit?: number
}

/** POST /api/pipelines/vram-budget 请求 */
export interface VramBudgetRequest {
  spec: PipelineSpec
}

/** VRAM 预算条目：峰值层单个节点的 VRAM 需求（§6.3） */
export interface VramBudgetItem {
  node_id: string
  mb: number
}

/**
 * 每设备 VRAM 预算条目（§6.3 每设备账本）。
 * 形状以 B3 后端为契约（仲裁 #28）：消费 `device_id` + `items` 峰值层明细。
 */
export interface VramDeviceBudget {
  /** 设备标识（如 "cuda:0"） */
  device_id: string
  total_mb: number | null
  /** 当前占用（来自 /api/devices） */
  used_mb: number | null
  /** 该管线在此设备的峰值 VRAM 需求 */
  pipeline_mb: number
  /** 峰值层的节点明细 */
  items: VramBudgetItem[]
  /** 是否超出预算（compute.allow_overcommit 决定是否放行执行） */
  over: boolean
}

/** POST /api/pipelines/vram-budget 响应（B3 形状为契约，仲裁 #28） */
export interface VramBudgetResponse {
  devices: VramDeviceBudget[]
  /** device="auto" 未分配池峰值层的节点明细 */
  unassigned: VramBudgetItem[]
  /** 未分配池峰值（MB；由调度器按 least_memory 落位） */
  unassigned_mb: number
  /** 是否允许超额提交（compute.allow_overcommit，放行策略由执行层决定） */
  allow_overcommit: boolean
}

// ===== 模型扩展操作（§5.2 / §8.1）=====

/** PUT /api/models/{m}/{mid}/tags 请求（§5.1 tag 存 meta） */
export interface ModelTagsRequest {
  tags: string[]
}

/** PUT /api/models/{m}/{mid}/variant 请求（变体单槽位切换 §5.2） */
export interface ModelVariantRequest {
  model_id: string
}

/** 变体切换响应（触发下载检查 + 重启提示） */
export interface ModelVariantResponse {
  ok: boolean
  /** 变体本地缺失，需先下载 */
  needs_download?: boolean
  /** 需重启模块生效 */
  needs_restart?: boolean
}
