import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import type { AppConfig } from '@/api/types'

/** setConfig 支持传入完整配置或基于当前配置的更新函数 */
export type ConfigUpdater = AppConfig | ((prev: AppConfig) => AppConfig)

export interface UseConfigResult {
  /** 当前配置（加载中为 null） */
  config: AppConfig | null
  /** 更新本地配置（不会立即提交到服务端） */
  setConfig: (updater: ConfigUpdater) => void
  /** 保存配置到服务端（PUT /api/config），成功返回 true */
  save: () => Promise<boolean>
  /** 立即持久化单项改动（不等保存按钮），成功返回 true */
  persistPartial: (apply: (serverLatest: AppConfig) => AppConfig) => Promise<boolean>
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

/** 应用配置管理：加载、编辑、保存与脏状态跟踪 */
export function useConfig(): UseConfigResult {
  const [config, setConfigState] = useState<AppConfig | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  /** 上次保存（或加载）的配置快照，用于脏检查 */
  const savedRef = useRef<string>('')

  const dirty = config !== null && JSON.stringify(config) !== savedRef.current

  const reload = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const cfg = await api.getConfig()
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

  const save = useCallback(async (): Promise<boolean> => {
    if (!config) return false
    setSaving(true)
    setError(null)
    try {
      // 以服务端返回结果校正本地缓存
      const saved = await api.putConfig(config)
      savedRef.current = JSON.stringify(saved)
      setConfigState(saved)
      return true
    } catch (e) {
      setError(toMessage(e))
      return false
    } finally {
      setSaving(false)
    }
  }, [config])

  /**
   * 立即持久化单项改动（如语言切换），不经过草稿保存按钮：
   * 以服务器最新配置为 PUT 基线，仅应用 `apply` 描述的改动，
   * 避免把本地其他未保存的草稿改动一起提交。
   * 成功后同步本地草稿与已保存快照中的对应字段，脏检查不受影响。
   */
  const persistPartial = useCallback(
    async (apply: (serverLatest: AppConfig) => AppConfig): Promise<boolean> => {
      setSaving(true)
      setError(null)
      try {
        const baseline = await api.getConfig()
        const saved = await api.putConfig(apply(baseline))
        // 快照更新为「原快照 + 同一改动」：只冲抵本次改动，保留其他草稿差异的脏状态
        // （极端情况下快照为空时，退化为服务端返回结果）
        savedRef.current = savedRef.current
          ? JSON.stringify(apply(JSON.parse(savedRef.current) as AppConfig))
          : JSON.stringify(saved)
        // 将同一改动应用到本地草稿（幂等：调用方通常已乐观更新）
        setConfigState((prev) => (prev ? apply(prev) : prev))
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
