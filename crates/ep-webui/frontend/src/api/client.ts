import type {
  AppConfig,
  BuiltinTemplatesResponse,
  DepReport,
  DeviceResponse,
  ScheduleInfo,
  StorageReport,
  ExecutePipelineRequest,
  ExecutePipelineResponse,
  ImportRequest,
  ImportResponse,
  LlmConnectionTestResult,
  LlmProvider,
  LlmProviderInput,
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
  StoreBrowseResponse,
  StoreListResponse,
  StoreLocateResponse,
  StoreResolveResponse,
  ScheduleListEntry,
  UnifiedEvent,
  WatchRule,
  WatchRuleInput,
  WebhookEntry,
  WebhookTargetInput,
  WebhookTriggerResponse,
} from './types'

/** apiFetch 默认超时（毫秒）：防挂死请求（如慢响应下的轮询堆积） */
const DEFAULT_TIMEOUT_MS = 30_000

/** 全局队列状态（T-15）：paused=true 时不再派发新任务，运行中任务不受影响 */
export interface QueueStatus {
  paused: boolean
  running: number
  queued: number
}

/** 访问保护令牌的 localStorage 键（T-06；仅存于当前浏览器） */
export const API_TOKEN_STORAGE_KEY = 'entrypoint.apiToken'

/** API 返回 401 时派发的事件：设置页监听后弹出令牌输入对话框 */
export const AUTH_REQUIRED_EVENT = 'entrypoint:auth-required'

/** 读取当前浏览器保存的访问令牌（未配置时返回空串） */
export function getApiToken(): string {
  try {
    return localStorage.getItem(API_TOKEN_STORAGE_KEY)?.trim() ?? ''
  } catch {
    // localStorage 不可用（隐私模式等）时视为未配置
    return ''
  }
}

/** 保存访问令牌；传空串 / null 清除 */
export function setApiToken(token: string | null): void {
  try {
    if (token?.trim()) {
      localStorage.setItem(API_TOKEN_STORAGE_KEY, token.trim())
    } else {
      localStorage.removeItem(API_TOKEN_STORAGE_KEY)
    }
  } catch {
    // localStorage 不可用时静默降级（本次会话无法持久化令牌）
  }
}

/**
 * 给浏览器原生加载的 URL（`<img>/<video>/<a download>`、window.open）追加
 * `?token=`：这些场景无法附加 Authorization 头。仅适用于 GET/HEAD 请求；
 * 未配置令牌时原样返回。
 */
export function withApiToken(url: string): string {
  const token = getApiToken()
  if (!token) return url
  const sep = url.includes('?') ? '&' : '?'
  return `${url}${sep}token=${encodeURIComponent(token)}`
}

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
  // T-06：访问保护令牌（调用方显式设置的 Authorization 优先）
  const token = getApiToken()
  if (token && !headers.has('Authorization')) {
    headers.set('Authorization', `Bearer ${token}`)
  }
  // 上传与调用方显式 signal 场景不叠加自动超时
  const needsTimeout =
    timeoutMs > 0 && !(init.body instanceof FormData) && init.signal === undefined
  const signal = needsTimeout ? AbortSignal.timeout(timeoutMs) : init.signal
  const resp = await fetch(`/api${path}`, { ...init, headers, signal })
  if (!resp.ok) {
    // 401：通知设置页弹出令牌输入（不在此处阻断 UI 流程）
    if (resp.status === 401) {
      window.dispatchEvent(new CustomEvent(AUTH_REQUIRED_EVENT))
    }
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
    xhr.open('POST', `/api/modules/${moduleId}/models/upload`)
    const token = getApiToken()
    if (token) xhr.setRequestHeader('Authorization', `Bearer ${token}`)
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
    const token = getApiToken()
    if (token) xhr.setRequestHeader('Authorization', `Bearer ${token}`)
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

  /**
   * 模块扩展端点代理（COMFYUI_BRIDGE_PLAN §3.5/§3.6；canonical /proxy，
   * 旧 /extra 为后端弃用别名）：daemon 把 /api/modules/{id}/proxy/{*path}
   * 原样转发给运行中适配器（模块未运行返回 409），支持 GET / POST(FormData)
   * / DELETE。path 按段做 URI 编码（如工作流文件名），调用方传原始字符串即可。
   */
  moduleProxy: <T>(
    moduleId: string,
    path: string,
    init?: ApiFetchOptions,
  ) =>
    apiFetch<T>(
      `/modules/${encodeURIComponent(moduleId)}/proxy/${path
        .split('/')
        .map(encodeURIComponent)
        .join('/')}`,
      init,
    ),

  // Config
  getConfig: () => apiFetch<AppConfig>('/config'),

  // Models
  models: () => apiFetch<ModelListResponse>('/models'),
  moduleModels: (id: string) =>
    apiFetch<ModelDetailResponse>(`/modules/${id}/models`),
  importModel: (moduleId: string, req: ImportRequest) =>
    apiFetch<ImportResponse>(`/modules/${moduleId}/models/import`, {
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
    return apiFetch<{ ok: boolean }>(`/modules/${moduleId}/models/upload`, {
      method: 'POST',
      body: form,
    })
  },

  /** 启动模型下载（source 缺省时由后端选择默认来源） */
  downloadModel: (    moduleId: string,
    body: { model_id: string; source?: ModelSource },
  ) =>
    apiFetch<{ ok: boolean }>(`/modules/${moduleId}/models/download`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  /** 当前全部模型下载任务的状态 */
  listModelDownloads: () =>
    apiFetch<ModelDownloadStatus[]>('/models/downloads'),

  /** 删除已下载的模型 */
  deleteModel: (moduleId: string, modelId: string) =>
    apiFetch<{ ok: boolean }>(
      `/modules/${moduleId}/models/${encodeURIComponent(modelId)}`,
      { method: 'DELETE' },
    ),

  /** 检查模型是否有可用更新 */
  checkModelUpdate: (moduleId: string, modelId: string) =>
    apiFetch<{ available: boolean; reason: string }>(
      `/modules/${moduleId}/models/${encodeURIComponent(modelId)}/check-update`,
      { method: 'POST' },
    ),

  /** 更新模型用户 tag（§5.1 / §8.1；tag 存 meta，随整合包流转） */
  setModelTags: (moduleId: string, modelId: string, body: ModelTagsRequest) =>
    apiFetch<{ ok: boolean }>(
      `/modules/${moduleId}/models/${encodeURIComponent(modelId)}/tags`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),

  /** 取消进行中的模型下载（§8.1，修 P2-6；409 = 无可取消的下载） */
  cancelModelDownload: (moduleId: string, modelId: string) =>
    apiFetch<{ ok: boolean }>(
      `/modules/${moduleId}/models/${encodeURIComponent(modelId)}/cancel-download`,
      { method: 'POST' },
    ),

  /** 切换模块激活变体（变体单槽位 §5.2；触发下载检查 + 重启提示） */
  setModelVariant: (
    moduleId: string,
    modelId: string,
    body: ModelVariantRequest,
  ) =>
    apiFetch<ModelVariantResponse>(
      `/modules/${moduleId}/models/${encodeURIComponent(modelId)}/variant`,
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
    withApiToken(
      `/api/tasks/${encodeURIComponent(taskId)}/artifacts/${encodeURIComponent(nodeId)}`,
    ),
  /**
   * 任务产物预览 URL（带 inline=1）：图片/视频/音频命中 daemon 白名单时
   * 以内联 Content-Disposition 返回，可直接喂给 <img>/<video>/<audio>；
   * 其余类型（含 html/svg）仍按下载处理。仅生成 URL，不走 fetch。
   */
  taskArtifactPreviewUrl: (taskId: string, nodeId: string) =>
    withApiToken(
      `/api/tasks/${encodeURIComponent(taskId)}/artifacts/${encodeURIComponent(nodeId)}?inline=1`,
    ),
  /** 全局队列状态（T-15）：运行/排队计数 + 是否暂停 */
  getQueueStatus: () => apiFetch<QueueStatus>('/queue'),
  /** 暂停队列（T-15）：只阻止新任务派发，运行中任务不受影响，排队任务保留 */
  pauseQueue: () => apiFetch<QueueStatus>('/queue/pause', { method: 'POST' }),
  /** 恢复队列（T-15）：立即尝试派发排队任务 */
  resumeQueue: () =>
    apiFetch<QueueStatus>('/queue/resume', { method: 'POST' }),

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
    withApiToken(`/api/pipelines/${encodeURIComponent(id)}/export`),
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
  /**
   * 执行已落盘管线（run 资源化 canonical：POST /pipelines/{id}/runs，
   * docs/API_DESIGN.md §运行资源）。wait 同步模式请求在服务端阻塞至终态
   * （可数分钟），不走默认 30s 超时。
   */
  executePipeline: (
    pipelineId: string,
    body: Omit<ExecutePipelineRequest, 'pipeline_id' | 'spec'>,
  ) =>
    apiFetch<ExecutePipelineResponse>(
      `/pipelines/${encodeURIComponent(pipelineId)}/runs`,
      {
        method: 'POST',
        body: JSON.stringify(body),
        timeoutMs: 0,
      },
    ),

  /**
   * 执行未落盘管线（内联 spec）：集合级 run 创建 POST /pipelines/runs，
   * body 携带 spec（与 pipeline_id 二选一，同传 400）。
   */
  executePipelineRun: (body: ExecutePipelineRequest) =>
    apiFetch<ExecutePipelineResponse>('/pipelines/runs', {
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
      `/pipelines/${encodeURIComponent(id)}/runs${suffix ? `?${suffix}` : ''}`,
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
  packExportUrl: (id: string) =>
    withApiToken(`/api/packs/${encodeURIComponent(id)}/export`),

  // 直跑（§5.3 / §8.1）
  // 提交走 hooks/use-direct-exec.ts 的 postExecuteSingle（需 AbortController
  // 长超时语义，不复用 apiFetch）；此处仅保留输入文件上传。

  /**
   * 上传输入文件到 workspace/uploads（浏览器端直跑输入；multipart 单文件）。
   * canonical POST /files（Stripe files 模式：上传换引用），旧 /upload/input
   * 为后端弃用别名。
   */
  uploadInput: (file: File) => {
    const form = new FormData()
    form.append('file', file)
    return apiFetch<UploadInputResponse>('/files', {
      method: 'POST',
      body: form,
    })
  },

  // Dependencies
  deps: () => apiFetch<DepReport>('/deps'),

  // 触发器（PLAN_TRIGGER_UNIFIED_LOG §5.3）
  /** 全部触发规则列表（含 recent 速览；兼容裸数组 / {watchers:[…]} 两种信封） */
  listWatchers: async () => {
    const resp = await apiFetch<WatchRule[] | { watchers?: WatchRule[] }>(
      '/watchers',
    )
    return Array.isArray(resp) ? resp : (resp.watchers ?? [])
  },
  /** 创建规则（body 不含 id，服务端生成 8 位十六进制），返回 {ok, id} */
  createWatcher: (body: WatchRuleInput) =>
    apiFetch<{ ok: boolean; id: string }>('/watchers', {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  /** 单条规则详情（未配置 → 404 apiCore.watcher.notFound） */
  getWatcher: (id: string) =>
    apiFetch<WatchRule>(`/watchers/${encodeURIComponent(id)}`),
  /** 全量更新（校验同 POST） */
  updateWatcher: (id: string, body: WatchRuleInput) =>
    apiFetch<{ ok: boolean }>(`/watchers/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  /** 删除规则 */
  deleteWatcher: (id: string) =>
    apiFetch<{ ok: boolean }>(`/watchers/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),

  // 统一事件日志（§5.7；倒序，最新在前）
  events: (query?: { rule?: string; type?: string; limit?: number }) => {
    const qs = new URLSearchParams()
    if (query?.rule) qs.set('rule', query.rule)
    if (query?.type) qs.set('type', query.type)
    if (query?.limit !== undefined) qs.set('limit', String(query.limit))
    const suffix = qs.toString()
    return apiFetch<{ events: UnifiedEvent[] }>(
      `/events${suffix ? `?${suffix}` : ''}`,
    )
  },

  // 内置 ffmpeg 模板库（T-13；侧栏 / 参数表单 / 执行器同源）
  /** 全部内置模板（GET /api/builtins；静态表投影） */
  builtinTemplates: () =>
    apiFetch<BuiltinTemplatesResponse>('/builtins'),

  // 存储水位（T-11；仪表盘存储卡片）
  /** 三目录已用字节数 + 模型缓存盘剩余空间（GET /api/storage） */
  storage: () => apiFetch<StorageReport>('/storage'),

  // 存储池逻辑名（T-26；计划 7 S0）
  /** 存储池列表（GET /api/stores；含解析后物理路径） */
  listStores: () => apiFetch<StoreListResponse>('/stores'),
  /** 目录浏览（GET /api/stores/{name}/browse?path=；单层，防穿越） */
  browseStore: (name: string, path = '') =>
    apiFetch<StoreBrowseResponse>(
      `/stores/${encodeURIComponent(name)}/browse?path=${encodeURIComponent(path)}`,
    ),
  /** store: 引用解析（POST /api/stores/resolve；缺池/禁用 → 422） */
  resolveStoreRef: (value: string) =>
    apiFetch<StoreResolveResponse>('/stores/resolve', {
      method: 'POST',
      body: JSON.stringify({ value }),
    }),
  /** 物理路径 → store: 逻辑名批量反向改写（TOML 导出前调用；池外为 null） */
  locateStorePaths: (paths: string[]) =>
    apiFetch<StoreLocateResponse>('/stores/locate', {
      method: 'POST',
      body: JSON.stringify({ paths }),
    }),

  // 定时计划一览（T-27 cron 双入口；管理仍走 /pipelines/{id}/schedule）
  /** 全部定时计划（GET /api/schedules） */
  listSchedules: () =>
    apiFetch<{ schedules: ScheduleListEntry[] }>('/schedules'),

  // Webhook 触发器（T-27；触发入口 POST /hooks/{id} 受全局访问保护覆盖）
  /** 全部 Webhook（GET /api/hooks） */
  listHooks: async () => {
    const resp = await apiFetch<WebhookEntry[] | { hooks?: WebhookEntry[] }>(
      '/hooks',
    )
    return Array.isArray(resp) ? resp : (resp.hooks ?? [])
  },
  /** 创建 Webhook（服务端生成 hook_id），返回 {ok, id} */
  createHook: (body: { name: string; enabled: boolean; target: WebhookTargetInput }) =>
    apiFetch<{ ok: boolean; id: string }>('/hooks', {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  /** 单条 Webhook（未配置 → 404） */
  getHook: (id: string) =>
    apiFetch<WebhookEntry>(`/hooks/${encodeURIComponent(id)}`),
  /** 全量更新（校验同 POST） */
  updateHook: (
    id: string,
    body: { name: string; enabled: boolean; target: WebhookTargetInput },
  ) =>
    apiFetch<{ ok: boolean }>(`/hooks/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  /** 删除（幂等） */
  deleteHook: (id: string) =>
    apiFetch<{ ok: boolean }>(`/hooks/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),
  /** 触发（POST /api/hooks/{id}；可选逐次覆盖 inputs/params/input_path） */
  triggerHook: (
    id: string,
    body?: { inputs?: Record<string, unknown>; params?: Record<string, unknown>; input_path?: string },
  ) =>
    apiFetch<WebhookTriggerResponse>(`/hooks/${encodeURIComponent(id)}`, {
      method: 'POST',
      body: JSON.stringify(body ?? {}),
    }),

  // LLM 提供商档案（T-24；设置页管理区，设置页独占消费）
  /** 脱敏视图列表（api_key 恒空串，是否已配置看 api_key_set） */
  listLlmProviders: () => apiFetch<LlmProvider[]>('/llm/providers'),
  /** 创建（重名 id → 409；校验失败 → 400） */
  createLlmProvider: (body: LlmProviderInput) =>
    apiFetch<LlmProvider>('/llm/providers', {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  /** 更新（id 不可改名；api_key 空串 = 保留，清除用 clear_api_key: true） */
  updateLlmProvider: (id: string, body: LlmProviderInput) =>
    apiFetch<LlmProvider>(`/llm/providers/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  /** 删除（未知 id → 404） */
  deleteLlmProvider: (id: string) =>
    apiFetch<{ ok: boolean }>(`/llm/providers/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),
  /**
   * 测试连接：daemon 代调 GET {base_url}/v1/models（超时由 profile
   * timeout_secs 决定，服务端恒返回 200 两态 → 不走客户端默认 30s 超时）。
   */
  testLlmProvider: (id: string) =>
    apiFetch<LlmConnectionTestResult>(
      `/llm/providers/${encodeURIComponent(id)}/test`,
      { method: 'POST', timeoutMs: 0 },
    ),
}
