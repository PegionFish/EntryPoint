import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import { wsManager } from '@/api/ws'
import type {
  PackBuildRequest,
  PackBuildResponse,
  PackImportResponse,
} from '@/api/types'

/** 单个整合包的导入/构建进度条目（WS pack_import 聚合） */
export interface PackProgressEntry {
  /** 当前阶段（accepted/extracting/…/done/build） */
  stage?: string
  /** 百分比 0-100；无法估算进度时缺失 */
  percent?: number
  /** running / completed / failed */
  state: string
  /** 阶段说明或错误信息 */
  message?: string
}

/** 浏览器上传进度 */
export interface PackUploadProgress {
  percent: number
  loaded: number
  total: number
}

/** 上传取消的内部错误标识（与语言无关，仅用于分支判断） */
export const PACK_UPLOAD_ABORTED = '__ep_pack_upload_aborted__'

/**
 * XHR 上传 .epzip（整合包可达数 GB，进度反馈必需；fetch 无上传进度）。
 * 表单字段名 `file`（后端 /api/packs/upload 契约）。
 */
export function uploadPackWithProgress(
  file: File,
  onProgress: (p: PackUploadProgress) => void,
): { promise: Promise<PackImportResponse>; abort: () => void } {
  const form = new FormData()
  form.append('file', file)
  const xhr = new XMLHttpRequest()
  const promise = new Promise<PackImportResponse>((resolve, reject) => {
    xhr.open('POST', '/api/packs/upload')
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
        resolve((body ?? {}) as PackImportResponse)
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
    xhr.addEventListener('abort', () => reject(new Error(PACK_UPLOAD_ABORTED)))
    xhr.send(form)
  })
  return { promise, abort: () => xhr.abort() }
}

/** 经 export 端点（302 + Content-Disposition attachment）触发浏览器下载 */
export function triggerPackDownload(packId: string): void {
  const a = document.createElement('a')
  a.href = api.packExportUrl(packId)
  a.download = ''
  document.body.appendChild(a)
  a.click()
  a.remove()
}

export interface UsePackIoOptions {
  /**
   * 进度到达终态（completed/failed）时回调一次：
   * 页面层负责 toast、刷新模块/模型列表等。
   */
  onSettled?: (packId: string, entry: PackProgressEntry) => void
}

export interface UsePackIoResult {
  /** 进度表（pack_id → 聚合条目；终态条目保留至 dismiss） */
  progress: Record<string, PackProgressEntry>
  /** 受理后登记初始进度条目 */
  trackProgress: (packId: string, stage?: string) => void
  /** 关闭（移除）终态进度条目 */
  dismiss: (packId: string) => void
  /** 本地路径导入（202 受理，进度走 WS） */
  importLocal: (path: string) => Promise<PackImportResponse>
  /** URL 导入（202 受理，进度走 WS） */
  importUrl: (url: string) => Promise<PackImportResponse>
  /**
   * 浏览器上传 .epzip（XHR 真实进度）；202 受理后自动登记进度条目。
   * 返回 promise 与 abort（关闭对话框时中止上传）。
   */
  upload: (
    file: File,
    onProgress: (p: PackUploadProgress) => void,
  ) => { promise: Promise<PackImportResponse>; abort: () => void }
  /**
   * 构建整合包（202 受理，进度走 WS stage="build"）。
   * autoDownload=true 时构建完成后自动触发 export 下载。
   */
  build: (
    req: PackBuildRequest,
    opts?: { autoDownload?: boolean },
  ) => Promise<PackBuildResponse>
  /** 卸载整合包（keepModels=true 保留包内安装的模型文件） */
  uninstall: (packId: string, keepModels: boolean) => Promise<{ ok: boolean }>
}

/**
 * 整合包导入/导出统一 I/O 状态机（信息架构 #47：导入/导出模块工具栏的底座）。
 *
 * - WS `pack_import` 订阅：按 pack_id 聚合进度；终态触发 onSettled；
 * - 构建受理时登记 autoDownload 的包，完成后自动触发 export 下载。
 */
export function usePackIo(options?: UsePackIoOptions): UsePackIoResult {
  const [progress, setProgress] = useState<Record<string, PackProgressEntry>>({})
  const onSettledRef = useRef(options?.onSettled)
  useEffect(() => {
    onSettledRef.current = options?.onSettled
  })
  /** 构建受理且要求完成后自动下载的 pack_id 集合 */
  const autoDownloadRef = useRef<Set<string>>(new Set())

  // WS pack_import 订阅：进度聚合 + 终态分发
  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'pack_import') return
      const entry: PackProgressEntry = {
        stage: msg.stage,
        percent: msg.percent,
        state: msg.state ?? 'running',
        message: msg.message,
      }
      setProgress((prev) => ({ ...prev, [msg.pack_id]: entry }))
      if (entry.state !== 'completed' && entry.state !== 'failed') return
      if (
        entry.state === 'completed' &&
        autoDownloadRef.current.has(msg.pack_id)
      ) {
        autoDownloadRef.current.delete(msg.pack_id)
        triggerPackDownload(msg.pack_id)
      }
      try {
        onSettledRef.current?.(msg.pack_id, entry)
      } catch {
        // 订阅方错误不影响状态机本身
      }
    })
  }, [])

  const trackProgress = useCallback((packId: string, stage?: string) => {
    setProgress((prev) => ({
      ...prev,
      [packId]: { state: 'running', stage, percent: 0 },
    }))
  }, [])

  const dismiss = useCallback((packId: string) => {
    setProgress((prev) => {
      if (!(packId in prev)) return prev
      const next = { ...prev }
      delete next[packId]
      return next
    })
  }, [])

  const importLocal = useCallback(
    async (path: string) => {
      const resp = await api.importPack({ source: 'local', path })
      trackProgress(resp.pack_id, 'accepted')
      return resp
    },
    [trackProgress],
  )

  const importUrl = useCallback(
    async (url: string) => {
      const resp = await api.importPack({ source: 'url', url })
      trackProgress(resp.pack_id, 'accepted')
      return resp
    },
    [trackProgress],
  )

  const upload = useCallback(
    (file: File, onProgress: (p: PackUploadProgress) => void) => {
      const { promise, abort } = uploadPackWithProgress(file, onProgress)
      // 受理成功后登记初始进度条目（WS 首条消息到达前即有反馈）
      const tracked = promise.then((resp) => {
        trackProgress(resp.pack_id, 'accepted')
        return resp
      })
      return { promise: tracked, abort }
    },
    [trackProgress],
  )

  const build = useCallback(
    async (req: PackBuildRequest, opts?: { autoDownload?: boolean }) => {
      const resp = await api.buildPack(req)
      if (opts?.autoDownload) autoDownloadRef.current.add(resp.pack_id)
      trackProgress(resp.pack_id, 'build')
      return resp
    },
    [trackProgress],
  )

  const uninstall = useCallback(
    (packId: string, keepModels: boolean) => api.deletePack(packId, keepModels),
    [],
  )

  return {
    progress,
    trackProgress,
    dismiss,
    importLocal,
    importUrl,
    upload,
    build,
    uninstall,
  }
}
