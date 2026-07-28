import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import type {
  DepReport,
  DeviceResponse,
  ModuleResponse,
  ModuleStatusResponse,
} from '@/api/types'

/** 仪表盘轮询间隔（毫秒） */
const POLL_INTERVAL_MS = 3000

export interface UseDevicesResult {
  devices: DeviceResponse[] | null
  modules: ModuleResponse[] | null
  deps: DepReport | null
  /** module_id → 实时状态（含端口 / 运行时长）。按模块并行拉取，单模块失败静默降级 */
  moduleStatus: Record<string, ModuleStatusResponse>
  loading: boolean
  error: string | null
}

/**
 * 仪表盘数据轮询 hook：
 * - 每 3 秒拉取设备 / 模块 / 系统依赖概览，并并行获取各模块实时状态（端口等）
 * - 页面不可见（document.hidden）时暂停轮询，恢复可见时立即刷新一次
 * - 上一次请求未完成时跳过本轮，避免慢响应下的请求堆积
 */
export function useDevices(): UseDevicesResult {
  const [devices, setDevices] = useState<DeviceResponse[] | null>(null)
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [deps, setDeps] = useState<DepReport | null>(null)
  const [moduleStatus, setModuleStatus] = useState<
    Record<string, ModuleStatusResponse>
  >({})
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const inFlight = useRef(false)

  const poll = useCallback(async () => {
    if (inFlight.current) return
    inFlight.current = true
    try {
      const [deviceList, moduleList, depReport] = await Promise.all([
        api.devices(),
        api.modules(),
        api.deps(),
      ])

      // 模块列表不含端口，按模块并行拉取实时状态；单模块失败不影响整体
      const statuses = await Promise.all(
        moduleList.map((m) => api.moduleStatus(m.id).catch(() => null)),
      )
      const statusMap: Record<string, ModuleStatusResponse> = {}
      for (const s of statuses) {
        if (s) statusMap[s.module_id] = s
      }

      setDevices(deviceList)
      setModules(moduleList)
      setDeps(depReport)
      setModuleStatus(statusMap)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      inFlight.current = false
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | null = null

    const start = () => {
      void poll()
      if (timer === null) {
        timer = setInterval(() => void poll(), POLL_INTERVAL_MS)
      }
    }
    const stop = () => {
      if (timer !== null) {
        clearInterval(timer)
        timer = null
      }
    }
    const onVisibilityChange = () => {
      if (document.hidden) {
        stop()
      } else {
        start()
      }
    }

    if (!document.hidden) start()
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => {
      stop()
      document.removeEventListener('visibilitychange', onVisibilityChange)
    }
  }, [poll])

  return { devices, modules, deps, moduleStatus, loading, error }
}
