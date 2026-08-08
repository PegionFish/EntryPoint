import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { api } from '@/api/client'
import { wsManager } from '@/api/ws'
import type {
  ModelDetailResponse,
  ModuleActionResult,
  ModuleResponse,
  ModuleStatusResponse,
} from '@/api/types'

/** 模块状态轮询间隔 */
const STATUS_POLL_MS = 3000
/** 前端日志缓冲上限（行） */
const MAX_LOG_LINES = 1000

export interface UseModuleDetailResult {
  /** 模块清单信息（名称 / 版本 / 类别 / 描述 / 路径） */
  module: ModuleResponse | null
  /** 实时运行状态（每 3 秒轮询） */
  status: ModuleStatusResponse | null
  /** 日志行（历史 + WebSocket 实时追加，上限 1000 行） */
  logs: string[]
  /** 该模块关联的模型详情 */
  models: ModelDetailResponse | null
  modelsLoading: boolean
  /** 首次加载中 */
  loading: boolean
  error: string | null
  /** 启动 / 停止操作进行中 */
  acting: boolean
  startModule: () => Promise<ModuleActionResult>
  stopModule: () => Promise<ModuleActionResult>
  /** 仅清空前端日志显示 */
  clearLogs: () => void
  /** 手动刷新状态 */
  refreshStatus: () => Promise<void>
}

function appendLine(prev: string[], line: string): string[] {
  const next =
    prev.length >= MAX_LOG_LINES
      ? prev.slice(prev.length - MAX_LOG_LINES + 1)
      : prev.slice()
  next.push(line)
  return next
}

/**
 * 模块详情数据源。
 *
 * - 每 3 秒轮询 `GET /api/modules/:id/status`（页面不可见时暂停）；
 * - 挂载时拉取历史日志与关联模型；
 * - 订阅全局 WebSocket，按 module_id 过滤实时日志；
 * - 启动 / 停止采用乐观更新：先切换为过渡态，请求完成后以服务端结果校正。
 */
export function useModuleDetail(moduleId: string | undefined): UseModuleDetailResult {
  const { t } = useTranslation('modules')
  const id = moduleId ?? ''
  const [module, setModule] = useState<ModuleResponse | null>(null)
  const [status, setStatus] = useState<ModuleStatusResponse | null>(null)
  const [logs, setLogs] = useState<string[]>([])
  const [models, setModels] = useState<ModelDetailResponse | null>(null)
  const [modelsLoading, setModelsLoading] = useState(true)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [acting, setActing] = useState(false)
  /**
   * 状态请求代数：每次 refreshStatus 自增，返回后与当前代数比对。
   * 路由 /modules/a→b 复用同一组件实例时，旧 id 的在途响应因代数
   * 不匹配被丢弃，不再覆盖新页状态（修跨 id 竞态，替代共享 mounted 标志）。
   */
  const statusGen = useRef(0)

  const refreshStatus = useCallback(async () => {
    if (!id) return
    const gen = ++statusGen.current
    try {
      const s = await api.moduleStatus(id)
      if (gen !== statusGen.current) return
      setStatus(s)
      setError(null)
    } catch (e) {
      if (gen !== statusGen.current) return
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [id])

  // 模块元信息（名称 / 版本 / 类别 / 描述）来自列表接口
  useEffect(() => {
    if (!id) return
    let cancelled = false
    api
      .modules()
      .then((list) => {
        if (cancelled) return
        const found = list.find((m) => m.id === id) ?? null
        setModule(found)
        if (!found) setError(t('error.moduleNotFound', { id }))
      })
      .catch(() => {
        /* 元信息加载失败不阻塞页面，状态轮询仍可进行 */
      })
    return () => {
      cancelled = true
    }
  }, [id, t])

  // 历史日志 + 关联模型：挂载时拉取一次
  useEffect(() => {
    if (!id) return
    let cancelled = false
    api
      .moduleLogs(id)
      .then((res) => {
        if (!cancelled) setLogs(res.lines ?? [])
      })
      .catch(() => {
        /* 日志拉取失败时保持空列表，等待实时日志 */
      })
    api
      .moduleModels(id)
      .then((res) => {
        if (!cancelled) setModels(res)
      })
      .catch(() => {
        /* 模型信息缺失时展示空态 */
      })
      .finally(() => {
        if (!cancelled) setModelsLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [id])

  // 状态轮询：3 秒一次，页面不可见时暂停。
  // 每次 id 变更以本地 cancelled 闭包终止旧轮询；在途响应由
  // refreshStatus 的代数比对丢弃（修跨 id 竞态）。
  useEffect(() => {
    if (!id) return
    let cancelled = false
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
        if (!document.hidden) void refreshStatus()
      }, STATUS_POLL_MS)
    }

    void refreshStatus().finally(() => {
      if (!cancelled) setLoading(false)
    })
    start()

    const onVisibility = () => {
      if (document.hidden) {
        stop()
      } else {
        void refreshStatus()
        start()
      }
    }
    document.addEventListener('visibilitychange', onVisibility)

    return () => {
      cancelled = true
      stop()
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [id, refreshStatus])

  // WebSocket 实时日志订阅（按 type='log' + module_id 过滤）
  useEffect(() => {
    if (!id) return
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'log' || msg.module_id !== id) return
      setLogs((prev) => appendLine(prev, msg.line))
    })
  }, [id])

  const startModule = useCallback(async () => {
    if (!id) throw new Error(t('error.missingModuleId'))
    setActing(true)
    // 乐观更新为过渡态
    setStatus((prev) => (prev ? { ...prev, status: 'starting' } : prev))
    try {
      const res = await api.startModule(id)
      if (res.error) throw new Error(res.error)
      await refreshStatus()
      return res
    } catch (e) {
      await refreshStatus() // 以服务端真实状态校正
      throw e instanceof Error ? e : new Error(String(e))
    } finally {
      setActing(false)
    }
  }, [id, refreshStatus, t])

  const stopModule = useCallback(async () => {
    if (!id) throw new Error(t('error.missingModuleId'))
    setActing(true)
    setStatus((prev) =>
      prev ? { ...prev, status: 'stopped', port: null, uptime_secs: 0 } : prev,
    )
    try {
      const res = await api.stopModule(id)
      if (res.error) throw new Error(res.error)
      await refreshStatus()
      return res
    } catch (e) {
      await refreshStatus()
      throw e instanceof Error ? e : new Error(String(e))
    } finally {
      setActing(false)
    }
  }, [id, refreshStatus, t])

  const clearLogs = useCallback(() => setLogs([]), [])

  return {
    module,
    status,
    logs,
    models,
    modelsLoading,
    loading,
    error,
    acting,
    startModule,
    stopModule,
    clearLogs,
    refreshStatus,
  }
}
