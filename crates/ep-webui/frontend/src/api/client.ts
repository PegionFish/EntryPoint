import type {
  AppConfig,
  DepReport,
  DeviceResponse,
  HealthResponse,
  ImportRequest,
  ImportResponse,
  ModelDetailResponse,
  ModelListResponse,
  ModuleActionResult,
  ModuleLogsResponse,
  ModuleResponse,
  ModuleStatusResponse,
} from './types'

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const resp = await fetch(`/api${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  })
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

  // Dependencies
  deps: () => apiFetch<DepReport>('/deps'),
}
