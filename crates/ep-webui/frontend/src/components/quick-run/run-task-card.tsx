import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { CircleStop, Download, Eye, EyeOff, Terminal, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type { TaskArtifact, TaskDetail } from '@/api/types'
import { wsManager } from '@/api/ws'
import {
  fetchArtifactPreview,
  IMAGE_PREVIEW_EXTS,
  TEXT_PREVIEW_EXTS,
  type ArtifactPreview,
} from '@/components/quick-run/artifact-preview'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'

/** 任务详情轮询间隔（毫秒），与直跑抽屉一致 */
const TASK_POLL_MS = 1500

/** 日志尾巴保留行数 */
const LOG_TAIL_LINES = 20

/** EP-PROGRESS 结构化进度行（adapter stdout → daemon log 流 → WS） */
const PROGRESS_RE = /^\[EP-PROGRESS\]\s+(\d{1,3})/

/** 退化 DAG 节点 id → 展示键（build_direct_pipeline 契约；两节点形态无 output） */
const NODE_LABEL_KEYS: Record<string, string> = {
  input: 'run:nodeInput',
  run: 'run:nodeRun',
  output: 'run:nodeOutput',
}

function isTerminal(status: string | null | undefined): boolean {
  return ['completed', 'failed', 'cancelled'].includes(
    (status ?? '').trim().toLowerCase(),
  )
}

/** 秒 → mm:ss（超 1h 进位 hh:mm:ss） */
function formatElapsed(total: number): string {
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  const mm = String(m).padStart(2, '0')
  const ss = String(s).padStart(2, '0')
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`
}

function statusBadgeClass(status: string): string {
  switch ((status ?? '').trim().toLowerCase()) {
    case 'completed':
      return 'border-status-running/30 bg-status-running/15 text-status-running'
    case 'failed':
      return 'border-status-error/30 bg-status-error/15 text-status-error'
    case 'queued':
    case 'running':
      return 'border-status-starting/30 bg-status-starting/15 text-status-starting'
    default:
      return 'border-status-preparing/30 bg-status-preparing/15 text-status-preparing'
  }
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/**
 * 快速调用 · 会话任务卡片（QUICK_RUN_PLAN D1 会话任务区）：
 * 自包含轮询 + WS progress 订阅，终态后拉取产物清单；
 * 支持取消、产物下载与文本/图片内联预览、任务中心深链。
 */
export function RunTaskCard({
  taskId,
  moduleId,
  moduleStatus,
  onDismiss,
}: {
  taskId: string
  /** 所属模块（D：日志尾巴 / A：进度流按 module_id 过滤） */
  moduleId?: string
  /** 模块当前服务状态（父级轮询下发；B：冷启动阶段徽章） */
  moduleStatus?: string | null
  onDismiss: () => void
}) {
  const { t } = useTranslation('run')
  const { t: tCommon } = useTranslation('common')
  const [task, setTask] = useState<TaskDetail | null>(null)
  const [artifacts, setArtifacts] = useState<TaskArtifact[]>([])
  const [preview, setPreview] = useState<ArtifactPreview | null>(null)
  const previewKeyRef = useRef<string | null>(null)
  // ── 状态可视化 ABCD ──
  /** adapter 上报的推理进度（0-100；EP-PROGRESS 日志流解析） */
  const [progressPct, setProgressPct] = useState<number | null>(null)
  /** 实时日志尾巴（最近 N 行） */
  const [logLines, setLogLines] = useState<string[]>([])
  const [showLogs, setShowLogs] = useState(false)
  const logBoxRef = useRef<HTMLPreElement | null>(null)
  /** 运行耗时（秒；started_running_at 起算） */
  const [elapsedSec, setElapsedSec] = useState<number | null>(null)

  // 轮询至终态 → 补拉产物
  useEffect(() => {
    let cancelled = false
    let timer: number | null = null
    const poll = async () => {
      try {
        const detail = await api.getTask(taskId)
        if (cancelled) return
        setTask(detail)
        if (isTerminal(detail.status)) {
          try {
            const list = await api.listTaskArtifacts(taskId)
            if (!cancelled) setArtifacts(list)
          } catch {
            // 产物拉取失败不阻塞状态呈现
          }
          return
        }
      } catch {
        // 单次失败静默，下轮重试
      }
      if (!cancelled) timer = window.setTimeout(poll, TASK_POLL_MS)
    }
    void poll()
    return () => {
      cancelled = true
      if (timer !== null) window.clearTimeout(timer)
    }
  }, [taskId])

  // WS progress：节点状态触发一次即时轮询；log 流解析 EP-PROGRESS + 尾巴
  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type === 'progress' && msg.task_id === taskId) {
        void api
          .getTask(taskId)
          .then((d) => !isTerminal(d.status) && setTask(d))
          .catch(() => {})
      }
      if (msg.type === 'log' && moduleId && msg.module_id === moduleId) {
        const m = PROGRESS_RE.exec(msg.line)
        if (m) {
          const pct = Math.min(100, Number.parseInt(m[1] ?? '', 10))
          if (Number.isFinite(pct)) {
            setProgressPct((prev) => (prev === null || pct > prev ? pct : prev))
          }
        }
        setLogLines((prev) => {
          const next = prev.length >= LOG_TAIL_LINES
            ? prev.slice(prev.length - LOG_TAIL_LINES + 1)
            : prev.slice()
          next.push(msg.line)
          return next
        })
      }
    })
  }, [taskId, moduleId])

  // 运行耗时（C）：running 中每秒跳；终态停表（保留最后值）
  useEffect(() => {
    if (!task || isTerminal(task.status)) return
    const started = task.started_running_at
      ? Date.parse(task.started_running_at)
      : NaN
    const base = Number.isFinite(started) ? started : Date.now()
    const tick = () => setElapsedSec(Math.max(0, Math.round((Date.now() - base) / 1000)))
    tick()
    const timer = window.setInterval(tick, 1000)
    return () => window.clearInterval(timer)
  }, [task])

  // 日志尾巴自动滚底
  useEffect(() => {
    if (showLogs && logBoxRef.current) {
      logBoxRef.current.scrollTop = logBoxRef.current.scrollHeight
    }
  }, [logLines, showLogs])

  // 预览对象 URL 生命周期
  useEffect(() => {
    return () => {
      if (preview?.objectUrl) URL.revokeObjectURL(preview.objectUrl)
    }
  }, [preview])

  async function handleCancel() {
    try {
      await api.cancelTask(taskId)
    } catch (e) {
      toast.error(errMsg(e))
    }
  }

  function togglePreview(a: TaskArtifact) {
    const key = `${a.node_id}/${a.name}`
    if (previewKeyRef.current === key) {
      previewKeyRef.current = null
      setPreview(null)
      return
    }
    previewKeyRef.current = key
    fetchArtifactPreview(api.taskArtifactUrl(taskId, a.node_id), a.node_id, a.name)
      .then(setPreview)
      .catch((e) => {
        previewKeyRef.current = null
        setPreview(null)
        toast.error(t('previewFail'), { description: errMsg(e) })
      })
  }

  const status = task?.status ?? null
  const terminal = isTerminal(status)

  return (
    <div className="space-y-2 rounded-xl border border-border bg-card p-3">
      {/* 头部：task id + 状态徽章 + 操作 */}
      <div className="flex items-center gap-2">
        <span className="truncate font-mono text-xs text-muted-foreground">
          {taskId}
        </span>
        <Badge variant="outline" className={statusBadgeClass(status ?? '')}>
          {status
            ? tCommon(`status.${status === 'queued' ? 'pending' : status}`)
            : '…'}
        </Badge>
        {/* B：冷启动阶段徽章（模块 preparing/starting 时任务必然未开跑） */}
        {!terminal &&
          moduleId &&
          ['preparing', 'starting'].includes(
            (moduleStatus ?? '').trim().toLowerCase(),
          ) && (
            <Badge
              variant="outline"
              className="border-status-starting/30 bg-status-starting/10 text-status-starting"
            >
              {t('status.preparingModule')}
            </Badge>
          )}
        {/* C：运行耗时 */}
        {elapsedSec !== null && !terminal && (
          <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
            {t('elapsed')} {formatElapsed(elapsedSec)}
          </span>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {!terminal && (
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="cancel"
              onClick={() => void handleCancel()}
            >
              <CircleStop className="size-3.5" />
            </Button>
          )}
          <Link
            to={`/tasks?focus=${encodeURIComponent(taskId)}`}
            className="text-xs text-primary hover:underline"
          >
            {t('openInTasks')}
          </Link>
          <Button variant="ghost" size="icon-sm" aria-label="dismiss" onClick={onDismiss}>
            <X className="size-3.5" />
          </Button>
        </div>
      </div>

      {/* A：推理进度条（EP-PROGRESS 日志流解析；running 且有进度时显示） */}
      {progressPct !== null && !terminal && (
        <div className="flex items-center gap-2">
          <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-primary transition-[width] duration-500"
              style={{ width: `${Math.min(100, Math.max(2, progressPct))}%` }}
            />
          </div>
          <span className="w-9 shrink-0 text-right font-mono text-[10px] tabular-nums text-muted-foreground">
            {progressPct}%
          </span>
        </div>
      )}

      {/* 节点级状态（退化 DAG：input/run[/output]） */}
      <div className="space-y-1">
        {(task?.nodes.length
          ? task.nodes.map((n) => n.node_id)
          : Object.keys(NODE_LABEL_KEYS)
        ).map((nodeId) => {
          const nodeState =
            task?.nodes.find((n) => n.node_id === nodeId)?.state ?? '—'
          return (
            <div
              key={nodeId}
              className="flex items-center justify-between gap-2 text-xs"
            >
              <span className="text-muted-foreground">
                {NODE_LABEL_KEYS[nodeId]
                  ? t(NODE_LABEL_KEYS[nodeId].slice(4))
                  : nodeId}
              </span>
              <span
                className={`font-mono ${
                  nodeState === 'completed'
                    ? 'text-status-running'
                    : nodeState === 'failed'
                      ? 'text-status-error'
                      : nodeState === 'running'
                        ? 'text-status-starting'
                        : ''
                }`}
              >
                {nodeState}
              </span>
            </div>
          )
        })}
      </div>

      {/* 错误信息 */}
      {task?.nodes
        .filter((n) => n.error)
        .map((n) => (
          <p key={n.node_id} className="break-all text-xs text-status-error">
            {n.node_id}: {n.error}
          </p>
        ))}
      {task?.error && (
        <p className="break-all text-xs text-status-error">{task.error}</p>
      )}

      {/* 产物列表（终态） */}
      {terminal && artifacts.length > 0 && (
        <div className="space-y-1.5 border-t border-border pt-2">
          <label className="text-sm font-medium">{t('artifacts')}</label>
          {artifacts.map((a) => {
            const previewable =
              TEXT_PREVIEW_EXTS.test(a.name) || IMAGE_PREVIEW_EXTS.test(a.name)
            return (
              <div key={`${a.node_id}/${a.name}`} className="flex items-center gap-2 text-xs">
                <span className="min-w-0 flex-1 truncate font-mono" title={a.name}>
                  {a.name}
                </span>
                {previewable && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-1.5"
                    onClick={() => togglePreview(a)}
                  >
                    {previewKeyRef.current === `${a.node_id}/${a.name}` ? (
                      <EyeOff className="size-3.5" />
                    ) : (
                      <Eye className="size-3.5" />
                    )}
                  </Button>
                )}
                <a
                  href={api.taskArtifactUrl(taskId, a.node_id)}
                  download={a.name}
                  className="inline-flex items-center gap-1 text-primary hover:underline"
                >
                  <Download className="size-3" />
                  {t('download')}
                </a>
              </div>
            )
          })}
          {/* 内联预览（文本 / 图片） */}
          {preview && (
            <div className="mt-1 max-h-64 overflow-auto rounded-lg border border-border bg-muted/30 p-2">
              {preview.kind === 'image' && preview.objectUrl ? (
                <img
                  src={preview.objectUrl}
                  alt={preview.name}
                  className="max-h-56 w-auto max-w-full rounded"
                />
              ) : preview.kind === 'text' ? (
                <pre className="whitespace-pre-wrap break-all font-mono text-[11px] leading-relaxed">
                  {preview.text}
                </pre>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {t('previewFail')} · {preview.name} ({preview.size} B)
                </p>
              )}
            </div>
          )}
        </div>
      )}

      {/* D：实时日志尾巴（该模块 stdout 的最近 N 行，默认折叠） */}
      {moduleId && logLines.length > 0 && (
        <div className="space-y-1 border-t border-border pt-2">
          <button
            type="button"
            onClick={() => setShowLogs((v) => !v)}
            className="inline-flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground"
          >
            <Terminal className="size-3.5" />
            {t('liveLogs')}
            <span className="font-mono text-[10px]">({logLines.length})</span>
          </button>
          {showLogs && (
            <pre
              ref={logBoxRef}
              className="max-h-40 overflow-auto rounded-lg border border-border bg-muted/30 p-2 font-mono text-[10px] leading-relaxed"
            >
              {logLines.join('\n')}
            </pre>
          )}
        </div>
      )}
    </div>
  )
}
