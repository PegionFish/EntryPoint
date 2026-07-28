import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import type { ModuleResponse, ModuleStatusResponse } from '@/api/types'

/** 模块列表轮询间隔 */
const POLL_INTERVAL_MS = 5000

export interface UseModulesResult {
  /** 模块清单（来自 GET /api/modules） */
  modules: ModuleResponse[]
  /** 每个模块的实时运行状态（端口 / 运行时长），键为 module_id */
  statusMap: Record<string, ModuleStatusResponse>
  /** 首次加载中 */
  loading: boolean
  /** 最近一次刷新失败的错误信息 */
  error: string | null
  /** 手动刷新 */
  refresh: () => Promise<void>
}

/**
 * 模块列表数据源。
 *
 * - 每 5 秒轮询 `GET /api/modules`；
 * - 列表返回后并行拉取各模块 `GET /api/modules/:id/status`，
 *   补齐列表接口不提供的端口与运行时长（allSettled，单个失败不影响整体）；
 * - 页面不可见（document.hidden）时暂停轮询，恢复可见时立即刷新。
 */
export function useModules(): UseModulesResult {
  const [modules, setModules] = useState<ModuleResponse[]>([])
  const [statusMap, setStatusMap] = useState<Record<string, ModuleStatusResponse>>({})
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const mounted = useRef(true)

  const refresh = useCallback(async () => {
    try {
      const list = await api.modules()
      // 并行获取各模块运行状态（端口 / uptime），失败时静默跳过
      const statuses = await Promise.allSettled(
        list.map((m) => api.moduleStatus(m.id)),
      )
      if (!mounted.current) return
      const map: Record<string, ModuleStatusResponse> = {}
      statuses.forEach((s, i) => {
        if (s.status === 'fulfilled') map[list[i].id] = s.value
      })
      setModules(list)
      setStatusMap(map)
      setError(null)
    } catch (e) {
      if (!mounted.current) return
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      if (mounted.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    mounted.current = true
    let timer: number | null = null

    const stop = () => {
      if (timer !== null) {
        window.clearInterval(timer)
        timer = null
      }
    }
    const start = () => {
      stop()
      timer = window.setInterval(() => {
        if (!document.hidden) void refresh()
      }, POLL_INTERVAL_MS)
    }

    void refresh()
    start()

    const onVisibility = () => {
      if (document.hidden) {
        stop()
      } else {
        void refresh()
        start()
      }
    }
    document.addEventListener('visibilitychange', onVisibility)

    return () => {
      mounted.current = false
      stop()
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [refresh])

  return { modules, statusMap, loading, error, refresh }
}
