import type {
  AppConfig,
  DepReport,
  DeviceResponse,
  ExecutePipelineRequest,
  ExecutePipelineResponse,
  HealthResponse,
  ImportRequest,
  ImportResponse,
  ModelDetailResponse,
  ModelDownloadStatus,
  ModelInfo,
  ModelListResponse,
  ModelSource,
  ModuleActionResult,
  ModuleLogsResponse,
  ModuleResponse,
  ModuleStatusResponse,
  PipelineSpec,
  PipelineSummary,
  TaskArtifact,
  TaskDetail,
  TaskSummary,
} from './types'

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const headers = new Headers(options?.headers)
  // FormData（文件上传）不手动设置 Content-Type，由浏览器自动附加 multipart 边界
  if (!(options?.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }
  const resp = await fetch(`/api${path}`, { ...options, headers })
  if (!resp.ok) {
    throw new Error(`API ${resp.status}: ${await resp.text()}`)
  }
  return resp.json()
}

export const api = {
  // Health
  health: () => apiFetch<HealthResponse>('/health'),

  // Devices
  devices: () => apiFetch<DeviceResponse[]>('/devices'),

  // Modules
  modules: () => apiFetch<ModuleResponse[]>('/modules'),
  moduleStatus: (id: string) =>
    apiFetch<ModuleStatusResponse>(`/modules/${id}/status`),
  moduleLogs: (id: string) =>
    apiFetch<ModuleLogsResponse>(`/modules/${id}/logs`),
  startModule: (id: string) =>
    apiFetch<ModuleActionResult>(`/modules/${id}/start`, { method: 'POST' }),
  stopModule: (id: string) =>
    apiFetch<ModuleActionResult>(`/modules/${id}/stop`, { method: 'POST' }),

  // Config
  getConfig: () => apiFetch<AppConfig>('/config'),
  putConfig: (cfg: AppConfig) =>
    apiFetch<AppConfig>('/config', {
      method: 'PUT',
      body: JSON.stringify(cfg),
    }),

  // Models
  models: () => apiFetch<ModelListResponse>('/models'),
  moduleModels: (id: string) =>
    apiFetch<ModelDetailResponse>(`/models/${id}`),
  importModel: (moduleId: string, req: ImportRequest) =>
    apiFetch<ImportResponse>(`/models/${moduleId}/import`, {
      method: 'POST',
      body: JSON.stringify(req),
    }),

  /** 启动模型下载（source 缺省时由后端选择默认来源） */
  downloadModel: (
    moduleId: string,
    body: { model_id: string; source?: ModelSource },
  ) =>
    apiFetch<{ ok: boolean }>(`/models/${moduleId}/download`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 当前全部模型下载任务的状态 */
  listModelDownloads: () =>
    apiFetch<ModelDownloadStatus[]>('/models/downloads'),

  /** 删除已下载的模型 */
  deleteModel: (moduleId: string, modelId: string) =>
    apiFetch<{ ok: boolean }>(
      `/models/${moduleId}/${encodeURIComponent(modelId)}`,
      { method: 'DELETE' },
    ),

  /** 检查模型是否有可用更新 */
  checkModelUpdate: (moduleId: string, modelId: string) =>
    apiFetch<{ available: boolean; reason: string }>(
      `/models/${moduleId}/${encodeURIComponent(modelId)}/check-update`,
      { method: 'POST' },
    ),

  /**
   * 上传本地模型文件（multipart/form-data）。
   * 表单字段：model_id + 每个文件一条 'files'，并按序附 'paths' 记录相对路径。
   */
  uploadModel: (
    moduleId: string,
    modelId: string,
    files: File[],
    paths: string[],
  ) => {
    const form = new FormData()
    form.append('model_id', modelId)
    files.forEach((file, i) => {
      form.append('files', file)
      form.append('paths', paths[i] || file.webkitRelativePath || file.name)
    })
    return apiFetch<ModelInfo>(`/models/${moduleId}/upload`, {
      method: 'POST',
      body: form,
    })
  },

  // Tasks
  listTasks: () => apiFetch<TaskSummary[]>('/tasks'),
  getTask: (taskId: string) =>
    apiFetch<TaskDetail>(`/tasks/${encodeURIComponent(taskId)}`),
  listTaskArtifacts: (taskId: string) =>
    apiFetch<TaskArtifact[]>(
      `/tasks/${encodeURIComponent(taskId)}/artifacts`,
    ),
  /** 任务产物下载 URL（直接用于 <a href> / window.open，不走 fetch） */
  taskArtifactUrl: (taskId: string, nodeId: string) =>
    `/api/tasks/${encodeURIComponent(taskId)}/artifacts/${encodeURIComponent(nodeId)}`,

  // Pipelines
  listPipelines: () => apiFetch<PipelineSummary[]>('/pipelines'),
  getPipeline: (id: string) =>
    apiFetch<PipelineSpec>(`/pipelines/${encodeURIComponent(id)}`),
  savePipeline: (id: string, spec: PipelineSpec) =>
    apiFetch<{ ok: boolean }>(`/pipelines/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(spec),
    }),
  deletePipeline: (id: string) =>
    apiFetch<{ ok: boolean }>(`/pipelines/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),
  executePipeline: (body: ExecutePipelineRequest) =>
    apiFetch<ExecutePipelineResponse>('/pipelines/execute', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  // Dependencies
  deps: () => apiFetch<DepReport>('/deps'),
}
