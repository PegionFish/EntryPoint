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
  ui: { scale_factor: number; font_size: number; dashboard_refresh_secs: number }
}

// ===== Models =====
export interface ModelInfo {
  model_id: string
  name: string
  target_dir: string
  status: string
  source: string
  size_estimate_mb: number
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
export interface DepReport {
  ffmpeg: {
    available: boolean
    version: string | null
    path: string | null
    guidance: string | null
  }
  torch_cuda: {
    module_id: string
    available: boolean
    cuda_version: string | null
    guidance: string | null
  }[]
}

// ===== WebSocket =====
export interface WsLogMessage {
  module_id: string
  line: string
}

export interface WsProgressMessage {
  pipeline_id: string
  node_id: string
  status: string
}
