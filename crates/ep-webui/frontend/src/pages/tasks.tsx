import { useCallback, useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  Activity,
  ChevronDown,
  CircleStop,
  Copy,
  Download,
  FileBox,
  GitBranch,
  Inbox,
  Loader2,
  Puzzle,
  RefreshCw,
  TriangleAlert,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import { api } from '@/api/client'
import type {
  ModuleResponse,
  ModuleStatusResponse,
  TaskArtifact,
  TaskDetail,
  TaskSummary,
} from '@/api/types'
import { wsManager } from '@/api/ws'
import { PageContainer } from '@/components/layout/page-container'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { statusMeta } from '@/lib/constants'
import { cn, formatBytes, formatUptime } from '@/lib/utils'

/** 任务列表轮询间隔（毫秒），与模块状态页保持一致 */
const POLL_INTERVAL = 5000

/** WS progress 消息节流窗口（毫秒）：一批进度消息只触发一次刷新 */
const WS_REFRESH_DELAY = 300

/** 处于运行 / 过渡态的模块视为「活跃」 */
const ACTIVE_STATUSES = new Set(['running', 'starting', 'preparing'])

/** 任务 / 节点状态的徽章与进度条配色（使用 index.css 语义色令牌） */
interface TaskStateMeta {
  /** 显示标签的 i18n 键（可含 common 跨命名空间键）；null 表示原样展示状态值 */
  labelKey: string | null
  dot: string
  badge: string
  bar: string
  pulse: boolean
}

const TASK_STATE_META: Record<string, TaskStateMeta> = {
  pending: {
    labelKey: 'common:status.pending',
    dot: 'bg-muted-foreground',
    badge: 'bg-muted text-muted-foreground border-border',
    bar: 'bg-muted-foreground',
    pulse: false,
  },
  running: {
    labelKey: 'common:status.running',
    dot: 'bg-status-starting',
    badge: 'bg-status-starting/15 text-status-starting border-status-starting/30',
    bar: 'bg-status-starting',
    pulse: true,
  },
  completed: {
    labelKey: 'common:status.completed',
    dot: 'bg-status-running',
    badge: 'bg-status-running/15 text-status-running border-status-running/30',
    bar: 'bg-status-running',
    pulse: false,
  },
  failed: {
    labelKey: 'common:status.failed',
    dot: 'bg-status-error',
    badge: 'bg-status-error/15 text-status-error border-status-error/30',
    bar: 'bg-status-error',
    pulse: false,
  },
  cancelled: {
    labelKey: 'common:status.cancelled',
    dot: 'bg-status-preparing',
    badge:
      'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
    bar: 'bg-status-preparing',
    pulse: false,
  },
  skipped: {
    labelKey: 'status.skipped',
    dot: 'bg-muted-foreground',
    badge: 'bg-muted text-muted-foreground border-border',
    bar: 'bg-muted-foreground',
    pulse: false,
  },
}

function taskStateMeta(state: string | null | undefined): TaskStateMeta {
  const key = (state ?? '').trim().toLowerCase()
  return (
    TASK_STATE_META[key] ?? {
      labelKey: null,
      dot: 'bg-muted-foreground',
      badge: 'bg-muted text-muted-foreground border-border',
      bar: 'bg-muted-foreground',
      pulse: false,
    }
  )
}

/** 终态任务不再变化（completed / failed / cancelled） */
function isTerminalStatus(status: string): boolean {
  return ['completed', 'failed', 'cancelled'].includes(
    status.trim().toLowerCase(),
  )
}

/** 任务开始时间戳格式（与前端支持的两种语言一一对应） */
const TASK_TIME_FORMAT_ZH = new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hourCycle: 'h23',
})
const TASK_TIME_FORMAT_EN = new Intl.DateTimeFormat('en', {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hourCycle: 'h23',
})

/** ISO 时间 → 本地化展示，例如 "2026-08-04 14:32:05" */
function formatTaskTime(iso: string, lang: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return (lang === 'en' ? TASK_TIME_FORMAT_EN : TASK_TIME_FORMAT_ZH).format(
    date,
  )
}

function failMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function isActive(m: ModuleResponse): boolean {
  return ACTIVE_STATUSES.has((m.service_status || m.status).toLowerCase())
}

/** 模块状态 → common 状态标签键（归一化规则与 constants.ts 的 statusMeta 一致） */
const MODULE_STATUS_KEYS: Record<string, string> = {
  running: 'common:status.running',
  stopped: 'common:status.stopped',
  starting: 'common:status.starting',
  preparing: 'common:status.preparing',
  error: 'common:status.error',
  not_ready: 'common:status.notReady',
}

function moduleStatusKey(status: string | null | undefined): string | null {
  if (!status) return null
  const key = status
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '_')
    .replace('notready', 'not_ready')
  return MODULE_STATUS_KEYS[key] ?? null
}

/** 模块分类 → tasks 命名空间分类标签键；未知分类原样展示 */
const CATEGORY_KEYS: Record<string, string> = {
  asr: 'category.asr',
  tts: 'category.tts',
  denoise: 'category.denoise',
  ocr: 'category.ocr',
  image: 'category.image',
  video: 'category.video',
  audio: 'category.audio',
  translate: 'category.translate',
  llm: 'category.llm',
  other: 'category.other',
}

function categoryKey(category: string): string | null {
  return CATEGORY_KEYS[category.toLowerCase()] ?? null
}

/** 模块状态徽章：圆点 + 翻译标签，过渡态带脉冲动画 */
function StatusBadge({ status }: { status: string }) {
  const { t } = useTranslation('tasks')
  const meta = statusMeta(status)
  const key = moduleStatusKey(status)
  const label =
    key !== null ? t(key) : status.trim() || t('common:status.unknown')
  return (
    <Badge variant="outline" className={meta.badge}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.transitional && 'animate-pulse',
        )}
      />
      {label}
    </Badge>
  )
}

/** 任务 / 节点状态徽章：圆点 + 翻译标签，运行中带脉冲动画 */
function TaskStateBadge({ state }: { state: string }) {
  const { t } = useTranslation('tasks')
  const meta = taskStateMeta(state)
  const label =
    meta.labelKey !== null
      ? t(meta.labelKey)
      : state || t('common:status.unknown')
  return (
    <Badge variant="outline" className={meta.badge}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.pulse && 'animate-pulse',
        )}
      />
      {label}
    </Badge>
  )
}

function SectionHeader({
  icon: Icon,
  title,
  count,
}: {
  icon: typeof Activity
  title: string
  count?: number
}) {
  return (
    <div className="mb-3 flex items-center gap-2">
      <Icon className="size-4 text-muted-foreground" />
      <h2 className="text-base font-semibold">{title}</h2>
      {count !== undefined && (
        <Badge
          variant="secondary"
          className="font-mono text-xs text-muted-foreground"
        >
          {count}
        </Badge>
      )}
    </div>
  )
}

function EmptyState({
  icon: Icon,
  title,
  hint,
  action,
}: {
  icon: typeof Activity
  title: string
  hint?: string
  action?: React.ReactNode
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border py-10 text-center">
      <Icon className="size-8 text-muted-foreground/50" />
      <p className="text-sm text-muted-foreground">{title}</p>
      {hint && <p className="text-xs text-muted-foreground/70">{hint}</p>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  )
}

/** 错误文本旁的复制按钮：写入剪贴板并给出 toast 反馈 */
function CopyIconButton({ text, label }: { text: string; label: string }) {
  const { t } = useTranslation('tasks')
  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text)
      toast.success(t('toast.copied', { label }))
    } catch {
      toast.error(t('toast.copyFailed'))
    }
  }
  const copyTitle = t('task.copy', { label })
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      className="shrink-0 text-status-error/70 hover:bg-status-error/15 hover:text-status-error"
      onClick={() => void handleCopy()}
      title={copyTitle}
      aria-label={copyTitle}
    >
      <Copy />
    </Button>
  )
}

/**
 * 单个任务卡片。
 *
 * - 头部：管线名、状态徽章、开始时间、耗时、节点进度条；
 * - 展开后：节点级详情（running 任务随轮询刷新）+ completed 任务的产物下载。
 */
function TaskCard({
  task,
  expanded,
  onToggle,
  now,
}: {
  task: TaskSummary
  expanded: boolean
  onToggle: () => void
  now: number
}) {
  const { t, i18n } = useTranslation('tasks')
  const [detail, setDetail] = useState<TaskDetail | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [detailRetry, setDetailRetry] = useState(0)
  const [artifacts, setArtifacts] = useState<TaskArtifact[] | null>(null)
  const [artifactsError, setArtifactsError] = useState<string | null>(null)
  const [artifactRetry, setArtifactRetry] = useState(0)

  const terminal = isTerminalStatus(task.status)

  // 展开时拉取节点详情；未终态的任务每轮询周期刷新一次
  useEffect(() => {
    if (!expanded) return
    let ignore = false
    let notified = false
    const load = async () => {
      try {
        const d = await api.getTask(task.id)
        if (ignore) return
        setDetail(d)
        setDetailError(null)
        notified = false
      } catch (e) {
        if (ignore) return
        setDetailError(failMsg(e))
        if (!notified) {
          notified = true
          toast.error(t('toast.taskDetailLoadFailed'), {
            description: failMsg(e),
          })
        }
      }
    }
    void load()
    if (terminal) return () => { ignore = true }
    const timer = setInterval(() => void load(), POLL_INTERVAL)
    return () => {
      ignore = true
      clearInterval(timer)
    }
  }, [expanded, task.id, terminal, detailRetry, t])

  // completed 任务拉取产物列表（状态转为 completed 或重试时触发）
  useEffect(() => {
    if (!expanded || task.status !== 'completed') return
    let ignore = false
    api
      .listTaskArtifacts(task.id)
      .then((list) => {
        if (ignore) return
        setArtifacts(list)
        setArtifactsError(null)
      })
      .catch((e) => {
        if (ignore) return
        setArtifactsError(failMsg(e))
        toast.error(t('toast.artifactsLoadFailed'), {
          description: failMsg(e),
        })
      })
    return () => {
      ignore = true
    }
  }, [expanded, task.id, task.status, artifactRetry, t])

  const meta = taskStateMeta(task.status)
  const startedMs = task.started_at ? Date.parse(task.started_at) : null
  const finishedMs = task.finished_at ? Date.parse(task.finished_at) : null
  const elapsedSecs =
    startedMs === null
      ? null
      : Math.max(
          0,
          Math.floor(
            ((terminal && finishedMs !== null ? finishedMs : now) -
              startedMs) /
              1000,
          ),
        )
  const percent =
    task.node_count > 0
      ? Math.min(100, Math.round((task.completed_nodes / task.node_count) * 100))
      : 0

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card transition-colors hover:border-primary/40">
      {/* 头部（点击展开 / 收起） */}
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className="w-full cursor-pointer px-4 py-3 text-left"
      >
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <ChevronDown
            className={cn(
              'size-4 shrink-0 text-muted-foreground transition-transform',
              !expanded && '-rotate-90',
            )}
          />
          <span className="min-w-0 flex-1 truncate text-sm font-medium">
            {task.pipeline_name || task.id}
          </span>
          <TaskStateBadge state={task.status} />
          <span className="font-mono text-xs text-muted-foreground">
            {startedMs !== null && task.started_at
              ? formatTaskTime(task.started_at, i18n.language)
              : '—'}
          </span>
          <span className="font-mono text-xs text-muted-foreground">
            {elapsedSecs === null
              ? `${t('common:label.duration')} —`
              : `${
                  terminal
                    ? t('common:label.duration')
                    : t('task.elapsed')
                } ${formatUptime(elapsedSecs)}`}
          </span>
        </div>
        <div className="mt-2 flex items-center gap-3">
          <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted">
            <div
              className={cn('h-full rounded-full transition-all', meta.bar)}
              style={{ width: `${percent}%` }}
            />
          </div>
          <span className="shrink-0 font-mono text-xs text-muted-foreground">
            {t('task.nodeProgress', {
              completed: task.completed_nodes,
              total: task.node_count,
            })}
          </span>
        </div>
      </button>

      {/* 展开区：节点详情 + 产物下载 */}
      {expanded && (
        <div className="border-t border-border">
          {detail === null && detailError === null && (
            <div className="flex items-center gap-2 px-4 py-3 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              {t('task.loadingDetail')}
            </div>
          )}

          {detailError !== null && (
            <div className="flex items-center gap-2 px-4 py-3 text-xs text-status-error">
              <TriangleAlert className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate">{detailError}</span>
              <Button
                variant="ghost"
                size="xs"
                onClick={() => setDetailRetry((n) => n + 1)}
              >
                {t('common:action.retry')}
              </Button>
            </div>
          )}

          {detail !== null && (
            <div className="divide-y divide-border/60">
              {detail.nodes.length === 0 && (
                <div className="px-4 py-3 text-xs text-muted-foreground">
                  {t('task.noNodes')}
                </div>
              )}
              {detail.nodes.map((n) => (
                <div key={n.node_id} className="px-4 py-2">
                  <div className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate font-mono text-xs">
                      {n.node_id}
                    </span>
                    <TaskStateBadge state={n.state} />
                  </div>
                  {n.error && (
                    <div className="mt-1.5 flex items-start gap-2 rounded-md border border-status-error/30 bg-status-error/10 px-2.5 py-2 text-xs text-status-error">
                      <TriangleAlert className="mt-px size-3.5 shrink-0" />
                      <span className="min-w-0 flex-1 break-all whitespace-pre-wrap">
                        {n.error}
                      </span>
                      <CopyIconButton
                        text={n.error}
                        label={t('task.errorMessage')}
                      />
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}

          {/* 产物下载：仅 completed 任务 */}
          {task.status === 'completed' && (
            <div className="border-t border-border/60 bg-muted/20 pb-1">
              <div className="flex items-center gap-2 px-4 pb-1 pt-3 text-xs font-medium text-muted-foreground">
                <FileBox className="size-3.5" />
                {t('artifacts.title')}
              </div>
              {artifacts === null && artifactsError === null && (
                <div className="flex items-center gap-2 px-4 py-2 text-xs text-muted-foreground">
                  <Loader2 className="size-3.5 animate-spin" />
                  {t('artifacts.loading')}
                </div>
              )}
              {artifactsError !== null && (
                <div className="flex items-center gap-2 px-4 py-2 text-xs text-status-error">
                  <TriangleAlert className="size-3.5 shrink-0" />
                  <span className="min-w-0 flex-1 truncate">
                    {artifactsError}
                  </span>
                  <Button
                    variant="ghost"
                    size="xs"
                    onClick={() => setArtifactRetry((n) => n + 1)}
                  >
                    {t('common:action.retry')}
                  </Button>
                </div>
              )}
              {artifacts !== null && artifacts.length === 0 && (
                <div className="px-4 py-2 text-xs text-muted-foreground">
                  {t('artifacts.empty')}
                </div>
              )}
              {artifacts?.map((a, i) => (
                <div
                  key={`${a.node_id}-${a.name}-${i}`}
                  className="flex items-center gap-3 px-4 py-2"
                >
                  <span
                    className="min-w-0 flex-1 truncate font-mono text-xs"
                    title={t('artifacts.node', { id: a.node_id })}
                  >
                    {a.name}
                  </span>
                  <span className="shrink-0 font-mono text-xs text-muted-foreground">
                    {formatBytes(a.size)}
                  </span>
                  <Button asChild variant="outline" size="xs">
                    <a
                      href={api.taskArtifactUrl(task.id, a.node_id)}
                      download
                    >
                      <Download />
                      {t('common:action.download')}
                    </a>
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

export function TasksPage() {
  const { t } = useTranslation('tasks')
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [statuses, setStatuses] = useState<
    Record<string, ModuleStatusResponse>
  >({})
  const [tasks, setTasks] = useState<TaskSummary[] | null>(null)
  const [tasksError, setTasksError] = useState<string | null>(null)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  // 轮询失败只在恢复前提示一次，避免每 5s 弹 toast
  const taskToastShown = useRef(false)

  const refresh = useCallback(async () => {
    try {
      const mods = await api.modules()
      setModules(mods)
      setError(null)
      // 仅活跃模块需要端口 / 运行时长明细
      const active = mods.filter(isActive)
      const results = await Promise.allSettled(
        active.map((m) => api.moduleStatus(m.id)),
      )
      setStatuses(() => {
        const next: Record<string, ModuleStatusResponse> = {}
        for (const r of results) {
          if (r.status === 'fulfilled') next[r.value.module_id] = r.value
        }
        return next
      })
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  const refreshTasks = useCallback(async () => {
    try {
      const list = await api.listTasks()
      // 按开始时间倒序（新任务在前）
      list.sort(
        (a, b) =>
          (b.started_at ? Date.parse(b.started_at) : 0) -
          (a.started_at ? Date.parse(a.started_at) : 0),
      )
      setTasks(list)
      setTasksError(null)
      taskToastShown.current = false
    } catch (e) {
      const msg = failMsg(e)
      setTasksError(msg)
      if (!taskToastShown.current) {
        taskToastShown.current = true
        toast.error(t('toast.taskListLoadFailed'), { description: msg })
      }
    }
  }, [t])

  useEffect(() => {
    void refresh()
    void refreshTasks()
    const timer = setInterval(() => {
      if (document.hidden) return
      void refresh()
      void refreshTasks()
    }, POLL_INTERVAL)
    return () => clearInterval(timer)
  }, [refresh, refreshTasks])

  // WS progress 消息：有任务在动时节流触发一次立即刷新
  useEffect(() => {
    let timer: number | null = null
    const unsubscribe = wsManager.onMessage((msg) => {
      if (msg.type !== 'progress') return
      if (timer !== null) return
      timer = window.setTimeout(() => {
        timer = null
        void refreshTasks()
      }, WS_REFRESH_DELAY)
    })
    return () => {
      unsubscribe()
      if (timer !== null) window.clearTimeout(timer)
    }
  }, [refreshTasks])

  // running/pending 任务存在时驱动「已运行 Xs」走针
  const hasActiveTask = (tasks ?? []).some(
    (item) => item.status === 'running' || item.status === 'pending',
  )
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!hasActiveTask) return
    setNow(Date.now())
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [hasActiveTask])

  const activeModules = (modules ?? []).filter(isActive)
  const runningCount = (modules ?? []).filter(
    (m) => (m.service_status || m.status).toLowerCase() === 'running',
  ).length

  return (
    <PageContainer
      title={t('page.title')}
      description={t('page.description')}
      actions={
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            void refresh()
            void refreshTasks()
          }}
        >
          <RefreshCw className="size-3.5" />
          {t('common:action.refresh')}
        </Button>
      }
    >
      <div className="space-y-8">
        {error && (
          <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
            <TriangleAlert className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">
              {t('page.loadFailed', { error })}
            </span>
            <Button variant="ghost" size="xs" onClick={() => void refresh()}>
              {t('common:action.retry')}
            </Button>
          </div>
        )}

        {/* ── 概览 ── */}
        <div className="flex flex-wrap items-center gap-4 rounded-lg border border-border bg-card px-6 py-4 sm:gap-8">
          <div>
            <div className="font-mono text-3xl font-bold text-status-running">
              {modules === null ? '–' : runningCount}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              {t('stats.runningServices')}
            </div>
          </div>
          <div className="hidden h-8 w-px bg-border sm:block" />
          <div>
            <div className="font-mono text-3xl font-bold">
              {modules === null ? '–' : modules.length}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              {t('stats.totalModules')}
            </div>
          </div>
          <div className="hidden h-8 w-px bg-border sm:block" />
          <div>
            <div className="font-mono text-3xl font-bold">
              {tasks === null ? '–' : tasks.length}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              {t('stats.pipelineTasks')}
            </div>
          </div>
        </div>

        {/* ── 运行中服务 ── */}
        <section>
          <SectionHeader
            icon={Activity}
            title={t('stats.runningServices')}
            count={activeModules.length}
          />
          {modules === null ? (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {Array.from({ length: 3 }).map((_, i) => (
                <Skeleton key={i} className="h-24 rounded-lg" />
              ))}
            </div>
          ) : activeModules.length === 0 ? (
            <EmptyState
              icon={CircleStop}
              title={t('services.emptyTitle')}
              hint={t('services.emptyHint')}
            />
          ) : (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {activeModules.map((m) => {
                const meta = statusMeta(m.service_status || m.status)
                const st = statuses[m.id]
                const catKey = categoryKey(m.category)
                return (
                  <div
                    key={m.id}
                    className="group rounded-lg border border-border bg-card p-4 transition-colors hover:border-primary/40"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex min-w-0 items-center gap-2">
                        <span
                          className={cn(
                            'size-2 shrink-0 rounded-full',
                            meta.dot,
                            meta.transitional && 'animate-pulse',
                          )}
                        />
                        <span className="truncate text-sm font-medium">
                          {m.name}
                        </span>
                      </div>
                      <Badge
                        variant="secondary"
                        className="shrink-0 text-xs text-muted-foreground"
                      >
                        {catKey !== null ? t(catKey) : m.category}
                      </Badge>
                    </div>
                    <div className="mt-3 flex items-center gap-4 font-mono text-xs text-muted-foreground">
                      {st?.port != null && (
                        <span>{t('services.port', { port: st.port })}</span>
                      )}
                      {st != null && st.uptime_secs > 0 && (
                        <span>
                          {t('services.uptime', {
                            uptime: formatUptime(st.uptime_secs),
                          })}
                        </span>
                      )}
                      {st == null && <span className="opacity-60">…</span>}
                    </div>
                  </div>
                )
              })}
            </div>
          )}
        </section>

        {/* ── 全部模块 ── */}
        <section>
          <SectionHeader
            icon={Puzzle}
            title={t('stats.totalModules')}
            count={modules?.length}
          />
          {modules === null ? (
            <div className="space-y-2">
              {Array.from({ length: 4 }).map((_, i) => (
                <Skeleton key={i} className="h-12 rounded-lg" />
              ))}
            </div>
          ) : modules.length === 0 ? (
            <EmptyState
              icon={Inbox}
              title={t('moduleTable.emptyTitle')}
              hint={t('moduleTable.emptyHint')}
            />
          ) : (
            <div className="overflow-hidden rounded-lg border border-border">
              <Table>
                <TableHeader>
                  <TableRow className="hover:bg-transparent">
                    <TableHead>{t('common:label.name')}</TableHead>
                    <TableHead>{t('moduleTable.category')}</TableHead>
                    <TableHead>{t('moduleTable.version')}</TableHead>
                    <TableHead>{t('common:label.status')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {modules.map((m) => {
                    const catKey = categoryKey(m.category)
                    return (
                      <TableRow key={m.id}>
                        <TableCell>
                          <div className="font-medium">{m.name}</div>
                          {m.description && (
                            <div className="max-w-md truncate text-xs text-muted-foreground">
                              {m.description}
                            </div>
                          )}
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {catKey !== null ? t(catKey) : m.category}
                        </TableCell>
                        <TableCell className="font-mono text-xs text-muted-foreground">
                          {m.version}
                        </TableCell>
                        <TableCell>
                          <StatusBadge status={m.service_status || m.status} />
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          )}
        </section>

        {/* ── 管线任务 ── */}
        <section>
          <SectionHeader
            icon={GitBranch}
            title={t('stats.pipelineTasks')}
            count={tasks?.length}
          />
          {tasksError && (
            <div className="mb-3 flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
              <TriangleAlert className="size-4 shrink-0" />
              <span className="min-w-0 flex-1 truncate">
                {t('tasks.listLoadFailed', { error: tasksError })}
              </span>
              <Button
                variant="ghost"
                size="xs"
                onClick={() => void refreshTasks()}
              >
                {t('common:action.retry')}
              </Button>
            </div>
          )}
          {tasks === null ? (
            <div className="space-y-2">
              {Array.from({ length: 2 }).map((_, i) => (
                <Skeleton key={i} className="h-20 rounded-lg" />
              ))}
            </div>
          ) : tasks.length === 0 ? (
            <EmptyState
              icon={GitBranch}
              title={t('tasks.emptyTitle')}
              hint={t('tasks.emptyHint')}
              action={
                <Button asChild variant="outline" size="sm">
                  <Link to="/pipeline">
                    <GitBranch className="size-3.5" />
                    {t('tasks.openEditor')}
                  </Link>
                </Button>
              }
            />
          ) : (
            <div className="space-y-3">
              {tasks.map((task) => (
                <TaskCard
                  key={task.id}
                  task={task}
                  now={now}
                  expanded={expandedId === task.id}
                  onToggle={() =>
                    setExpandedId((cur) => (cur === task.id ? null : task.id))
                  }
                />
              ))}
            </div>
          )}
        </section>
      </div>
    </PageContainer>
  )
}
