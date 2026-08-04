import type {
  AppConfig,
  DepReport,
  DeviceResponse,
  DirectExecRequest,
  DirectExecResponse,
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
  ModelTagsRequest,
  ModelVariantRequest,
  ModelVariantResponse,
  ModuleActionResult,
  ModuleLogsResponse,
  ModuleResponse,
  ModuleStatusResponse,
  PackBuildRequest,
  PackBuildResponse,
  PackDetail,
  PackImportRequest,
  PackImportResponse,
  PackInfo,
  PipelineSpec,
  PipelineSummary,
  PipelineTasksQuery,
  TaskArtifact,
  TaskDetail,
  TaskSummary,
  UploadInputResponse,
  VramBudgetRequest,
  VramBudgetResponse,
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

  /** 更新模型用户 tag（§5.1 / §8.1；tag 存 meta，随整合包流转） */
  setModelTags: (moduleId: string, modelId: string, body: ModelTagsRequest) =>
    apiFetch<{ ok: boolean }>(
      `/models/${moduleId}/${encodeURIComponent(modelId)}/tags`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),

  /** 取消进行中的模型下载（§8.1，修 P2-6；409 = 无可取消的下载） */
  cancelModelDownload: (moduleId: string, modelId: string) =>
    apiFetch<{ ok: boolean }>(
      `/models/${moduleId}/${encodeURIComponent(modelId)}/cancel-download`,
      { method: 'POST' },
    ),

  /** 切换模块激活变体（变体单槽位 §5.2；触发下载检查 + 重启提示） */
  setModelVariant: (
    moduleId: string,
    modelId: string,
    body: ModelVariantRequest,
  ) =>
    apiFetch<ModelVariantResponse>(
      `/models/${moduleId}/${encodeURIComponent(modelId)}/variant`,
      { method: 'PUT', body: JSON.stringify(body) },
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

  /** 管线级任务列表（§6.8，修 P1-5；含 queued 状态与队列位置） */
  pipelineTasks: (id: string, query?: PipelineTasksQuery) => {
    const qs = new URLSearchParams()
    if (query?.status) qs.set('status', query.status)
    if (query?.limit !== undefined) qs.set('limit', String(query.limit))
    const suffix = qs.toString()
    return apiFetch<TaskSummary[]>(
      `/pipelines/${encodeURIComponent(id)}/tasks${suffix ? `?${suffix}` : ''}`,
    )
  },

  /** 每设备 VRAM 预算分解（§6.3 编辑器实时计算） */
  vramBudget: (body: VramBudgetRequest) =>
    apiFetch<VramBudgetResponse>('/pipelines/vram-budget', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  // Packs（整合包 §4 / §8.1）
  /** 已安装整合包列表（注册表） */
  listPacks: () => apiFetch<PackInfo[]>('/packs'),

  /** 整合包详情（内容清单 / 适配报告） */
  getPack: (id: string) =>
    apiFetch<PackDetail>(`/packs/${encodeURIComponent(id)}`),

  /** 导入整合包：本地路径或 URL（202；进度走 WS pack_import 消息） */
  importPack: (body: PackImportRequest) =>
    apiFetch<PackImportResponse>('/packs/import', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 上传整合包 .epzip（multipart 单文件；202 同 importPack） */
  uploadPack: (file: File) => {
    const form = new FormData()
    form.append('file', file)
    return apiFetch<PackImportResponse>('/packs/upload', {
      method: 'POST',
      body: form,
    })
  },

  /** 卸载整合包（keepModels=true 保留包内安装的模型） */
  deletePack: (id: string, keepModels = false) =>
    apiFetch<{ ok: boolean }>(
      `/packs/${encodeURIComponent(id)}${keepModels ? '?keep_models=true' : ''}`,
      { method: 'DELETE' },
    ),

  /** 构建整合包（§4.5；202，构建完成后可下载 .epzip） */
  buildPack: (body: PackBuildRequest) =>
    apiFetch<PackBuildResponse>('/packs/build', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 整合包 .epzip 下载 URL（直接用于 <a href> / window.open，不走 fetch） */
  packExportUrl: (id: string) => `/api/packs/${encodeURIComponent(id)}/export`,

  // 直跑（§5.3 / §8.1）
  /** 单模型直跑（202；模块未运行时后端自动拉起并等健康） */
  executeSingle: (body: DirectExecRequest) =>
    apiFetch<DirectExecResponse>('/execute/single', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 上传输入文件到 workspace/uploads（浏览器端直跑输入；multipart 单文件） */
  uploadInput: (file: File) => {
    const form = new FormData()
    form.append('file', file)
    return apiFetch<UploadInputResponse>('/upload/input', {
      method: 'POST',
      body: form,
    })
  },

  // Dependencies
  deps: () => apiFetch<DepReport>('/deps'),
}
