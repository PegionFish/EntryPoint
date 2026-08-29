import type {
  AppConfig,
  DepReport,
  DeviceResponse,
  ScheduleInfo,
  ExecutePipelineRequest,
  ExecutePipelineResponse,
  ImportRequest,
  ImportResponse,
  ModelDetailResponse,
  ModelDownloadStatus,
  ModelListResponse,
  ModelSource,
  ModelTagsRequest,
  ModelVariantRequest,
  ModelVariantResponse,
  ModuleActionResult,
  ModuleImportSummary,
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

/** apiFetch 默认超时（毫秒）：防挂死请求（如慢响应下的轮询堆积） */
const DEFAULT_TIMEOUT_MS = 30_000

interface ApiFetchOptions extends RequestInit {
  /**
   * 请求超时（毫秒）；0 表示不限时（wait 同步执行等长任务显式 opt-out）。
   * 默认 30s；FormData 上传（可达数 GB）自动跳过超时。
   */
  timeoutMs?: number
}

async function apiFetch<T>(path: string, options?: ApiFetchOptions): Promise<T> {
  const headers = new Headers(options?.headers)
  // FormData（文件上传）不手动设置 Content-Type，由浏览器自动附加 multipart 边界
  if (!(options?.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }
  const { timeoutMs = DEFAULT_TIMEOUT_MS, ...init } = options ?? {}
  // 上传与调用方显式 signal 场景不叠加自动超时
  const needsTimeout =
    timeoutMs > 0 && !(init.body instanceof FormData) && init.signal === undefined
  const signal = needsTimeout ? AbortSignal.timeout(timeoutMs) : init.signal
  const resp = await fetch(`/api${path}`, { ...init, headers, signal })
  if (!resp.ok) {
    throw new Error(`API ${resp.status}: ${await resp.text()}`)
  }
  return resp.json()
}

/**
 * 模型文件上传（XHR 真实进度）。fetch 无上传进度，模型可达数 GB，
 * 进度反馈必需（参照 use-pack-io 的 XHR 进度模式）。错误形状与
 * apiFetch 一致（`API <status>: <body>`），便于统一解析。
 */
export function uploadModelWithProgress(
  moduleId: string,
  modelId: string,
  files: File[],
  paths: string[] | undefined,
  onProgress: (p: { loaded: number; total: number; percent: number }) => void,
): Promise<{ ok: boolean }> {
  const form = new FormData()
  form.append('model_id', modelId)
  for (const f of files) form.append('files', f)
  if (paths) {
    for (const p of paths) form.append('paths', p)
  }
  return new Promise<{ ok: boolean }>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/models/${moduleId}/upload`)
    xhr.upload.addEventListener('progress', (e) => {
      if (!e.lengthComputable) return
      onProgress({
        loaded: e.loaded,
        total: e.total,
        percent: e.total > 0 ? Math.min(100, (e.loaded / e.total) * 100) : 0,
      })
    })
    xhr.addEventListener('load', () => {
      let body: unknown = null
      try {
        body = JSON.parse(xhr.responseText)
      } catch {
        // 非 JSON 响应
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve({ ok: true })
        return
      }
      const msg =
        body && typeof body === 'object' && 'error' in body
          ? (body as { error: unknown }).error
          : null
      reject(
        new Error(
          typeof msg === 'string' && msg.trim() ? msg : `HTTP ${xhr.status}`,
        ),
      )
    })
    xhr.addEventListener('error', () => reject(new Error('network error')))
    xhr.send(form)
  })
}

/**
 * 模块标准档案导入上传（HETERO_DIST_PLAN §2.3 POST /api/modules/import）。
 * XHR 真实进度（同 uploadModelWithProgress 模式）；错误形状与 apiFetch
 * 一致：抛出 `API <status>: <body>`，body 含 {"error","code"}。
 */
export function uploadModuleArchive(
  file: File,
  onProgress?: (p: { loaded: number; total: number; percent: number }) => void,
): Promise<ModuleImportSummary> {
  const form = new FormData()
  form.append('file', file)
  return new Promise<ModuleImportSummary>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', '/api/modules/import')
    xhr.upload.addEventListener('progress', (e) => {
      if (!onProgress || !e.lengthComputable) return
      onProgress({
        loaded: e.loaded,
        total: e.total,
        percent: e.total > 0 ? Math.min(100, (e.loaded / e.total) * 100) : 0,
      })
    })
    xhr.addEventListener('load', () => {
      let body: unknown = null
      try {
        body = JSON.parse(xhr.responseText)
      } catch {
        // 非 JSON 响应
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(body as ModuleImportSummary)
        return
      }
      reject(new Error(`API ${xhr.status}: ${xhr.responseText}`))
    })
    xhr.addEventListener('error', () => reject(new Error('network error')))
    xhr.send(form)
  })
}

export const api = {
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

  // Models
  models: () => apiFetch<ModelListResponse>('/models'),
  moduleModels: (id: string) =>
    apiFetch<ModelDetailResponse>(`/models/${id}`),
  importModel: (moduleId: string, req: ImportRequest) =>
    apiFetch<ImportResponse>(`/models/${moduleId}/import`, {
      method: 'POST',
      body: JSON.stringify(req),
    }),

  /**
   * 浏览器上传模型文件（§6.3：文件夹多文件 / 单个 .zip/.tar.gz/.tgz 归档）。
   * 本地网络场景不做尺寸限制（后端 DefaultBodyLimit 已禁用，模型可达数 GB）。
   */
  uploadModel: (
    moduleId: string,
    modelId: string,
    files: File[],
    paths?: string[],
  ) => {
    const form = new FormData()
    form.append('model_id', modelId)
    for (const f of files) form.append('files', f)
    if (paths) {
      for (const p of paths) form.append('paths', p)
    }
    return apiFetch<{ ok: boolean }>(`/models/${moduleId}/upload`, {
      method: 'POST',
      body: form,
    })
  },

  /** 启动模型下载（source 缺省时由后端选择默认来源） */
  downloadModel: (    moduleId: string,
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

  // Tasks
  listTasks: () => apiFetch<TaskSummary[]>('/tasks'),
  getTask: (taskId: string) =>
    apiFetch<TaskDetail>(`/tasks/${encodeURIComponent(taskId)}`),
  listTaskArtifacts: (taskId: string) =>
    apiFetch<TaskArtifact[]>(
      `/tasks/${encodeURIComponent(taskId)}/artifacts`,
    ),
  /**
   * 取消任务（P1-11；daemon 路由 POST /tasks/{id}/cancel）。
   * 排队中 → 立即终结不执行；运行中 → 逻辑终态 cancelled。
   * 404 任务不存在 / 409 已是终态。
   */
  cancelTask: (taskId: string) =>
    apiFetch<{ ok: boolean; status: string }>(
      `/tasks/${encodeURIComponent(taskId)}/cancel`,
      { method: 'POST' },
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
  /** 导出分享 JSON（自包含信封，entrypoint-pipeline/v1） */
  exportPipelineUrl: (id: string) =>
    `/api/pipelines/${encodeURIComponent(id)}/export`,
  importPipelineShare: (jsonText: string) =>
    apiFetch<{ ok: boolean; id: string; name: string }>('/pipelines/import', {
      method: 'POST',
      body: jsonText,
      headers: { 'Content-Type': 'application/json' },
    }),
  // 定时调度（cron）
  getSchedule: (id: string) =>
    apiFetch<ScheduleInfo>(`/pipelines/${encodeURIComponent(id)}/schedule`),
  putSchedule: (
    id: string,
    body: { cron: string; enabled?: boolean; inputs?: unknown; params?: unknown },
  ) =>
    apiFetch<{ ok: boolean }>(`/pipelines/${encodeURIComponent(id)}/schedule`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  deleteSchedule: (id: string) =>
    apiFetch<{ ok: boolean }>(`/pipelines/${encodeURIComponent(id)}/schedule`, {
      method: 'DELETE',
    }),
  executePipeline: (body: ExecutePipelineRequest) =>
    // wait 同步模式请求在服务端阻塞至终态（可数分钟），不走默认 30s 超时
    apiFetch<ExecutePipelineResponse>('/pipelines/execute', {
      method: 'POST',
      body: JSON.stringify(body),
      timeoutMs: 0,
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

  /** 上传整合包 .zip（multipart 单文件；202 同 importPack） */
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

  /** 构建整合包（§4.5；202，构建完成后可下载 .zip） */
  buildPack: (body: PackBuildRequest) =>
    apiFetch<PackBuildResponse>('/packs/build', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 整合包 .zip 下载 URL（直接用于 <a href> / window.open，不走 fetch） */
  packExportUrl: (id: string) => `/api/packs/${encodeURIComponent(id)}/export`,

  // 直跑（§5.3 / §8.1）
  // 提交走 hooks/use-direct-exec.ts 的 postExecuteSingle（需 AbortController
  // 长超时语义，不复用 apiFetch）；此处仅保留输入文件上传。

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
