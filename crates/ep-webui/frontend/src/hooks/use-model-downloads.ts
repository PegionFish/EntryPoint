import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import { wsManager } from '@/api/ws'
import type { ModelDownloadState, ModelSource } from '@/api/types'

/**
 * 下载进度条目。
 *
 * 与后端 `ModelDownloadStatus` 基本一致，但 `source` 为可选：
 * WS `model_download` 推送消息不携带 source 字段，合并时沿用列表接口的值。
 */
export interface ModelDownloadProgress {
  module_id: string
  model_id: string
  source?: ModelSource
  /** 下载进度百分比（0-100） */
  percent: number
  /** 已下载字节数 */
  bytes: number
  state: ModelDownloadState
}

/** 兜底轮询间隔：3s，且仅在存在 downloading 条目时轮询 */
const POLL_INTERVAL_MS = 3000

/** 下载条目键：`{module_id}/{model_id}` */
export function downloadKey(moduleId: string, modelId: string): string {
  return `${moduleId}/${modelId}`
}

function isTerminal(state: ModelDownloadState): boolean {
  return state === 'completed' || state === 'failed' || state === 'cancelled'
}

export interface UseModelDownloadsResult {
  /** 全部下载条目（key = `{module_id}/{model_id}`），含历史记录 */
  downloads: Record<string, ModelDownloadProgress>
  /** 按模块 / 模型取条目 */
  get: (moduleId: string, modelId: string) => ModelDownloadProgress | undefined
  /** 手动拉取下载列表（发起下载后立即同步用） */
  refresh: () => Promise<void>
}

/**
 * 页面级模型下载状态机。
 *
 * - 主通道：订阅全局 WS 的 `model_download` 推送；
 * - 兜底通道：挂载时拉取一次 `listModelDownloads` 基线对齐，
 *   之后每 3s 轮询一次，但仅在存在 `downloading` 条目时才真正发请求；
 * - `onSettled`：条目由 `downloading` 转入终态（completed / failed / cancelled）
 *   时触发一次，供调用方刷新模型列表并弹结果 toast。
 *   挂载基线中的历史终态、以及同一任务的重复终态消息都不会重复触发。
 */
export function useModelDownloads(
  onSettled?: (entry: ModelDownloadProgress) => void,
): UseModelDownloadsResult {
  const [downloads, setDownloads] = useState<Record<string, ModelDownloadProgress>>({})
  const downloadsRef = useRef(downloads)
  /** 已通知过终态的条目，防止 WS 与轮询双通道重复触发 */
  const settledNotifiedRef = useRef<Set<string>>(new Set())
  const onSettledRef = useRef(onSettled)
  useEffect(() => {
    onSettledRef.current = onSettled
  })

  /** 合并一批条目并检测 downloading → 终态 的迁移 */
  const applyEntries = useCallback((entries: ModelDownloadProgress[]) => {
    const prev = downloadsRef.current
    const next = { ...prev }
    const settled: ModelDownloadProgress[] = []
    for (const entry of entries) {
      const key = downloadKey(entry.module_id, entry.model_id)
      const before = prev[key]
      // WS 推送不带 source：沿用此前列表接口给出的值
      next[key] = entry.source ? entry : { ...entry, source: before?.source }
      if (entry.state === 'downloading') {
        // 新的下载任务开始：允许其终态再次触发通知
        settledNotifiedRef.current.delete(key)
      } else if (
        isTerminal(entry.state) &&
        before?.state === 'downloading' &&
        !settledNotifiedRef.current.has(key)
      ) {
        settledNotifiedRef.current.add(key)
        settled.push(next[key])
      }
    }
    downloadsRef.current = next
    setDownloads(next)
    for (const entry of settled) {
      try {
        onSettledRef.current?.(entry)
      } catch {
        // 订阅方错误不影响状态机本身
      }
    }
  }, [])

  /** 拉取后端下载列表并合并 */
  const refresh = useCallback(async () => {
    try {
      const list = await api.listModelDownloads()
      applyEntries(list)
    } catch {
      // 基线 / 轮询失败静默忽略：WS 主通道仍可工作
    }
  }, [applyEntries])

  // 挂载时基线对齐
  useEffect(() => {
    void refresh()
  }, [refresh])

  // WS 主通道
  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'model_download') return
      applyEntries([
        {
          module_id: msg.module_id,
          model_id: msg.model_id,
          percent: msg.percent,
          bytes: msg.bytes,
          state: msg.state,
        },
      ])
    })
  }, [applyEntries])

  // 轮询兜底：仅在有 downloading 条目时每 3s 轮询
  useEffect(() => {
    const timer = window.setInterval(() => {
      const hasDownloading = Object.values(downloadsRef.current).some(
        (d) => d.state === 'downloading',
      )
      if (!hasDownloading) return
      void refresh()
    }, POLL_INTERVAL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])

  const get = useCallback(
    (moduleId: string, modelId: string) => downloads[downloadKey(moduleId, modelId)],
    [downloads],
  )

  return { downloads, get, refresh }
}
