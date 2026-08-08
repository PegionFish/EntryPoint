import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import { wsManager } from '@/api/ws'
import type {
  DirectExecRequest,
  DirectExecResponse,
  TaskArtifact,
  TaskDetail,
} from '@/api/types'

/**
 * 直跑（§5.3）提交的 fetch 超时（毫秒）。
 *
 * 后端 `/api/execute/single` 同步等待模块自动拉起并健康
 * （ready_timeout 默认 30s；首次运行还需准备 venv，可能数分钟），
 * 故给足超时余量，UI 侧同时展示「正在启动模块…」提示。
 */
export const DIRECT_EXEC_SUBMIT_TIMEOUT_MS = 300_000

/** 任务详情轮询间隔（毫秒） */
const TASK_POLL_MS = 1500

/** 任务是否处于终态（completed / failed / cancelled） */
export function isTaskTerminal(status: string | null | undefined): boolean {
  return ['completed', 'failed', 'cancelled'].includes(
    (status ?? '').trim().toLowerCase(),
  )
}

/**
 * 直跑提交（带 AbortController 超时）。
 *
 * 不复用 apiFetch 的原因：需要 signal 控制长超时，且超时错误
 * 需与一般网络错误区分（UI 提示「模块启动等待超时」）。
 * 错误形状与 apiFetch 一致（`API <status>: <body>`），便于统一解析。
 */
async function postExecuteSingle(
  body: DirectExecRequest,
): Promise<DirectExecResponse> {
  const controller = new AbortController()
  const timer = window.setTimeout(
    () => controller.abort(),
    DIRECT_EXEC_SUBMIT_TIMEOUT_MS,
  )
  try {
    const resp = await fetch('/api/execute/single', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: controller.signal,
    })
    if (!resp.ok) {
      throw new Error(`API ${resp.status}: ${await resp.text()}`)
    }
    return (await resp.json()) as DirectExecResponse
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') {
      throw new Error('__ep_direct_exec_submit_timeout__')
    }
    throw e
  } finally {
    window.clearTimeout(timer)
  }
}

export interface UseDirectExecResult {
  /** 提交进行中（含后端同步等待模块拉起） */
  submitting: boolean
  /** 提交错误（原始消息，UI 层解析展示） */
  submitError: string | null
  /** 已受理任务 id */
  taskId: string | null
  /** 任务详情（轮询更新，终态后停止） */
  task: TaskDetail | null
  /** 任务产物清单（终态后拉取） */
  artifacts: TaskArtifact[]
  /** WS progress 按 task_id 过滤出的节点状态（node_id → status） */
  nodeProgress: Record<string, string>
  /** 提交直跑任务；成功返回 task_id，失败返回 null（错误见 submitError） */
  submit: (req: DirectExecRequest) => Promise<string | null>
  /** 重置全部状态（关闭抽屉 / 开始新一轮前调用） */
  reset: () => void
}

/**
 * 单模型直跑状态机（§5.3）：
 *
 * 提交（长超时 fetch）→ 202 task_id → 轮询任务详情 + WS progress
 * 按 task_id 过滤节点状态 → 终态后拉取产物清单。
 *
 * 组件卸载 / reset 后轮询与订阅自动停止。
 */
export function useDirectExec(): UseDirectExecResult {
  const [submitting, setSubmitting] = useState(false)
  const [submitError, setSubmitError] = useState<string | null>(null)
  const [taskId, setTaskId] = useState<string | null>(null)
  const [task, setTask] = useState<TaskDetail | null>(null)
  const [artifacts, setArtifacts] = useState<TaskArtifact[]>([])
  const [nodeProgress, setNodeProgress] = useState<Record<string, string>>({})
  const mounted = useRef(true)
  /**
   * 提交代数：每次 submit / reset 自增。在途响应返回后与当前代数比对，
   * 过期（已有更新的提交）直接丢弃，防止旧响应覆盖新提交状态（P1）。
   */
  const submitGen = useRef(0)

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])

  const reset = useCallback(() => {
    submitGen.current += 1
    setSubmitting(false)
    setSubmitError(null)
    setTaskId(null)
    setTask(null)
    setArtifacts([])
    setNodeProgress({})
  }, [])

  const submit = useCallback(async (req: DirectExecRequest) => {
    const gen = ++submitGen.current
    setSubmitError(null)
    setTask(null)
    setArtifacts([])
    setNodeProgress({})
    setSubmitting(true)
    try {
      const resp = await postExecuteSingle(req)
      if (gen !== submitGen.current || !mounted.current) return null
      setTaskId(resp.task_id)
      return resp.task_id
    } catch (e) {
      if (gen !== submitGen.current || !mounted.current) return null
      setSubmitError(e instanceof Error ? e.message : String(e))
      return null
    } finally {
      if (gen === submitGen.current && mounted.current) setSubmitting(false)
    }
  }, [])

  // 任务详情轮询：task_id 存在且未终态时每 1.5s 拉取一次；
  // 终态落定后补拉产物清单并停止轮询。
  useEffect(() => {
    if (!taskId) return
    let cancelled = false
    let timer: number | null = null

    const poll = async () => {
      try {
        const detail = await api.getTask(taskId)
        if (cancelled || !mounted.current) return
        setTask(detail)
        if (isTaskTerminal(detail.status)) {
          try {
            const list = await api.listTaskArtifacts(taskId)
            if (!cancelled && mounted.current) setArtifacts(list)
          } catch {
            // 产物拉取失败不阻塞状态呈现
          }
          return // 终态：不再续订轮询
        }
      } catch {
        // 单次轮询失败静默忽略，下一轮重试
      }
      if (!cancelled) timer = window.setTimeout(poll, TASK_POLL_MS)
    }

    void poll()
    return () => {
      cancelled = true
      if (timer !== null) window.clearTimeout(timer)
    }
  }, [taskId])

  // WS progress 订阅：仅采纳当前 task_id 的节点进度（修 P2-7 并发串染）
  useEffect(() => {
    if (!taskId) return
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'progress' || msg.task_id !== taskId) return
      setNodeProgress((prev) => ({ ...prev, [msg.node_id]: msg.status }))
    })
  }, [taskId])

  return {
    submitting,
    submitError,
    taskId,
    task,
    artifacts,
    nodeProgress,
    submit,
    reset,
  }
}
