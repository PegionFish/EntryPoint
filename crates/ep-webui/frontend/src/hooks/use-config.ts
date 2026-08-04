import { useCallback, useEffect, useRef, useState } from 'react'
import type { AppConfig } from '@/api/types'

/* ── 类型（§8.2/§8.3 契约；api/types.ts 为 S2 所有权，扩展字段本地补充，
      入 types.ts 的对齐已列为仲裁请求）────────────────────────────── */

/** AppConfig 超集：§8.3 新增字段（服务器序列化恒含，类型标可选以容错） */
export interface AppConfigExt extends AppConfig {
  python: AppConfig['python'] & { uv_cache_dir?: string; constraints?: string }
  compute: AppConfig['compute'] & {
    cuda_libs_dir?: string
    single_device?: string | null
  }
  packs?: { staging_dir?: string }
  /** 每模块激活模型变体（单槽位 §5.2）：module_id → model_id */
  active_models?: Record<string, string>
}

/** PUT /api/config 响应（§8.2）：合并后完整配置 + requires_restart 平级 */
export type PutConfigResponse = AppConfigExt & { requires_restart: boolean }

/**
 * 深度合并补丁（§8.2）：分区 → 字段两级局部。
 * 服务器对缺省字段保留原值；`active_models` 按键合并（键删除不可表达，
 * 合并语义限制，见 C7 交付报告）。
 */
export type ConfigPatch = Partial<{
  [K in keyof AppConfigExt]: Partial<NonNullable<AppConfigExt[K]>>
}>

/** setConfig 支持传入完整配置或基于当前配置的更新函数 */
export type ConfigUpdater = AppConfigExt | ((prev: AppConfigExt) => AppConfigExt)

export interface UseConfigResult {
  /** 当前配置（加载中为 null） */
  config: AppConfigExt | null
  /** 更新本地配置（不会立即提交到服务端） */
  setConfig: (updater: ConfigUpdater) => void
  /**
   * 保存配置到服务端（PUT /api/config 深度合并语义，§8.2）。
   * 仅提交相对已保存快照的最小补丁，避免整替覆盖并发写入
   * （如统一页切换变体写 active_models）。成功返回响应（含
   * requires_restart），失败返回 null。
   */
  save: () => Promise<PutConfigResponse | null>
  /** 立即持久化补丁（不等保存按钮；主题/语言等即时切换），成功返回 true */
  persistPartial: (patch: ConfigPatch) => Promise<boolean>
  /** 重新从服务端拉取配置（丢弃未保存的本地修改） */
  reload: () => Promise<void>
  /** 首次加载中 */
  loading: boolean
  /** 保存请求进行中 */
  saving: boolean
  /** 最近一次加载 / 保存的错误信息 */
  error: string | null
  /** 本地配置与上次保存的配置不一致 */
  dirty: boolean
}

function toMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/* ── 原始请求辅助 ─────────────────────────────────────────────────
   PUT 已改为「任意 JSON patch 深度合并」（§8.2），请求/响应形状不再
   匹配 api/client.ts（S2 所有权）现有 `putConfig(cfg: AppConfig)`
   签名；本地直连 /api/config，行为与 apiFetch 一致（非 2xx 抛错）。 */

/** 拉取服务器全量配置（GET /api/config） */
export async function fetchServerConfig(): Promise<AppConfigExt> {
  const resp = await fetch('/api/config')
  if (!resp.ok) {
    throw new Error(`API ${resp.status}: ${await resp.text()}`)
  }
  return (await resp.json()) as AppConfigExt
}

/** 提交配置补丁（PUT /api/config 深度合并），返回合并后配置 + 重启标记 */
export async function putConfigPatch(
  patch: ConfigPatch,
): Promise<PutConfigResponse> {
  const resp = await fetch('/api/config', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  })
  if (!resp.ok) {
    throw new Error(`API ${resp.status}: ${await resp.text()}`)
  }
  return (await resp.json()) as PutConfigResponse
}

/** 从 PUT 响应剥离 requires_restart，得到纯配置对象 */
function stripRestartFlag(resp: PutConfigResponse): AppConfigExt {
  const out: Record<string, unknown> = { ...resp }
  delete out.requires_restart
  return out as unknown as AppConfigExt
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/**
 * 计算 base → next 的最小合并补丁（两级：分区 → 字段）。
 * - 字段值按 JSON 序列化比较（数组整体替换语义，与服务端 merge 一致）
 * - active_models 按键比较：仅输出新增/变更键（合并语义无法表达删键）
 */
export function diffConfig(base: AppConfigExt, next: AppConfigExt): ConfigPatch {
  const patch: Record<string, unknown> = {}
  const nextObj = next as unknown as Record<string, unknown>
  const baseObj = base as unknown as Record<string, unknown>
  for (const key of Object.keys(nextObj)) {
    const n = nextObj[key]
    if (n === undefined) continue
    const b = baseObj[key]
    if (isPlainObject(n)) {
      const baseSection = isPlainObject(b) ? b : {}
      const sectionPatch: Record<string, unknown> = {}
      for (const field of Object.keys(n)) {
        if (JSON.stringify(n[field]) !== JSON.stringify(baseSection[field])) {
          sectionPatch[field] = n[field]
        }
      }
      if (Object.keys(sectionPatch).length > 0) patch[key] = sectionPatch
    } else if (JSON.stringify(n) !== JSON.stringify(b)) {
      patch[key] = n
    }
  }
  return patch as ConfigPatch
}

/** 将两级补丁应用到完整配置（savedRef 快照与本地草稿共用） */
function applyPatch(cfg: AppConfigExt, patch: ConfigPatch): AppConfigExt {
  const next: Record<string, unknown> = { ...cfg }
  const cfgObj = cfg as unknown as Record<string, unknown>
  for (const [section, fields] of Object.entries(patch)) {
    if (fields === undefined) continue
    const base = cfgObj[section]
    next[section] = {
      ...(isPlainObject(base) ? base : {}),
      ...(fields as Record<string, unknown>),
    }
  }
  return next as unknown as AppConfigExt
}

/** 应用配置管理：加载、编辑、保存（合并补丁）与脏状态跟踪 */
export function useConfig(): UseConfigResult {
  const [config, setConfigState] = useState<AppConfigExt | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  /** 上次保存（或加载）的配置快照，用于脏检查与最小补丁基线 */
  const savedRef = useRef<string>('')

  const dirty = config !== null && JSON.stringify(config) !== savedRef.current

  const reload = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const cfg = await fetchServerConfig()
      savedRef.current = JSON.stringify(cfg)
      setConfigState(cfg)
    } catch (e) {
      setError(toMessage(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void reload()
  }, [reload])

  const setConfig = useCallback((updater: ConfigUpdater) => {
    setConfigState((prev) => {
      if (prev === null) return prev
      return typeof updater === 'function' ? updater(prev) : updater
    })
  }, [])

  const save = useCallback(async (): Promise<PutConfigResponse | null> => {
    if (!config) return null
    setSaving(true)
    setError(null)
    try {
      // active_models 空键/空值行不落盘（占位行；合并语义不支持删键）
      const cleaned: AppConfigExt = config.active_models
        ? {
            ...config,
            active_models: Object.fromEntries(
              Object.entries(config.active_models).filter(
                ([k, v]) => k.trim() !== '' && v.trim() !== '',
              ),
            ),
          }
        : config
      const baseline: AppConfigExt = savedRef.current
        ? (JSON.parse(savedRef.current) as AppConfigExt)
        : cleaned
      const patch = diffConfig(baseline, cleaned)
      if (Object.keys(patch).length === 0) {
        return { ...cleaned, requires_restart: false }
      }
      const resp = await putConfigPatch(patch)
      // 以服务端合并结果校正本地缓存与已保存快照
      const cfgOnly = stripRestartFlag(resp)
      savedRef.current = JSON.stringify(cfgOnly)
      setConfigState(cfgOnly)
      return resp
    } catch (e) {
      setError(toMessage(e))
      return null
    } finally {
      setSaving(false)
    }
  }, [config])

  /**
   * 立即持久化单项改动（如语言/主题切换），不经过草稿保存按钮：
   * 直接提交最小补丁（PUT 合并语义），成功后把同一补丁应用到本地
   * 草稿与已保存快照，只冲抵本次改动的脏状态。
   */
  const persistPartial = useCallback(
    async (patch: ConfigPatch): Promise<boolean> => {
      setSaving(true)
      setError(null)
      try {
        const resp = await putConfigPatch(patch)
        const cfgOnly = stripRestartFlag(resp)
        // 快照更新为「原快照 + 同一补丁」：只冲抵本次改动，保留其他草稿差异的脏状态
        savedRef.current = savedRef.current
          ? JSON.stringify(
              applyPatch(JSON.parse(savedRef.current) as AppConfigExt, patch),
            )
          : JSON.stringify(cfgOnly)
        // 将同一补丁应用到本地草稿（幂等：调用方通常已乐观更新）
        setConfigState((prev) => (prev ? applyPatch(prev, patch) : prev))
        return true
      } catch (e) {
        setError(toMessage(e))
        return false
      } finally {
        setSaving(false)
      }
    },
    [],
  )

  return {
    config,
    setConfig,
    save,
    persistPartial,
    reload,
    loading,
    saving,
    error,
    dirty,
  }
}
