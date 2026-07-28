import { useCallback, useEffect, useState } from 'react'
import { api } from '@/api/client'
import type {
  ImportRequest,
  ImportResponse,
  ModelDetailResponse,
  ModelListResponse,
} from '@/api/types'

export interface UseModelsResult {
  /** 按模块分组的模型列表（加载中为 null） */
  models: ModelListResponse | null
  /** 已获取的模块模型详情缓存（module_id → 详情） */
  details: Record<string, ModelDetailResponse>
  /** 拉取指定模块的模型详情（文件数、实际大小、缓存路径） */
  moduleModels: (moduleId: string) => Promise<ModelDetailResponse>
  /** 导入本地模型，成功后自动刷新列表与详情 */
  importModel: (
    moduleId: string,
    req: ImportRequest,
  ) => Promise<ImportResponse>
  /** 刷新模型列表 */
  refresh: () => Promise<void>
  loading: boolean
  error: string | null
}

/** 模型列表 / 详情 / 导入管理 */
export function useModels(): UseModelsResult {
  const [models, setModels] = useState<ModelListResponse | null>(null)
  const [details, setDetails] = useState<Record<string, ModelDetailResponse>>(
    {},
  )
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      setError(null)
      setModels(await api.models())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    setLoading(true)
    void refresh()
  }, [refresh])

  const moduleModels = useCallback(async (moduleId: string) => {
    const resp = await api.moduleModels(moduleId)
    setDetails((prev) => ({ ...prev, [moduleId]: resp }))
    return resp
  }, [])

  const importModel = useCallback(
    async (moduleId: string, req: ImportRequest) => {
      const resp = await api.importModel(moduleId, req)
      if (!resp.error) {
        // 导入成功后刷新列表与该模块详情（均容错，不阻塞结果返回）
        await Promise.allSettled([refresh(), moduleModels(moduleId)])
      }
      return resp
    },
    [refresh, moduleModels],
  )

  return { models, details, moduleModels, importModel, refresh, loading, error }
}
