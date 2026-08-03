// ===== Health =====
export interface HealthResponse {
  status: string
  version: string
}

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
  }
  ports: { range_start: number; range_end: number }
  models: {
    cache_dir: string
    hf_endpoint: string
    default_source: string
    max_concurrent_downloads: number
    cache_paths: string[]
  }
  python: { path: string; uv_path: string }
  pipeline: {
    max_parallel: number
    default_timeout_secs: number
    keep_workspace: boolean
    workspace_dir: string
  }
  network: { http_proxy: string; https_proxy: string; no_proxy: string }
  ui: { scale_factor: number; font_size: number; dashboard_refresh_secs: number }
}

// ===== Models =====
/** 模型来源：HuggingFace / ModelScope / 自定义 URL */
export type ModelSource = 'huggingface' | 'modelscope' | 'url'

/** 模型下载状态机（WS 推送与下载列表共用） */
export type ModelDownloadState =
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

/** /ws 聚合消息（按 type 判别） */
export type WsMessage =
  | WsLogMessage
  | WsProgressMessage
  | WsModelDownloadMessage

// ===== Tasks =====
export interface TaskSummary {
  id: string
  pipeline_name: string
  status: string
  started_at?: string
  finished_at?: string
  node_count: number
  completed_nodes: number
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
  params: Record<string, unknown>
  position?: { x: number; y: number }
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
}

export interface ExecutePipelineResponse {
  task_id: string
}
