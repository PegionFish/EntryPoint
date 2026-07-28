import { useCallback, useEffect, useState } from 'react'
import {
  Activity,
  CircleStop,
  GitBranch,
  Inbox,
  Puzzle,
  RefreshCw,
  TriangleAlert,
} from 'lucide-react'
import { api } from '@/api/client'
import type {
  ModuleResponse,
  ModuleStatusResponse,
} from '@/api/types'
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
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn, formatUptime } from '@/lib/utils'

/** 轮询间隔（毫秒），与模块状态页保持一致 */
const POLL_INTERVAL = 5000

/** 处于运行 / 过渡态的模块视为「活跃」 */
const ACTIVE_STATUSES = new Set(['running', 'starting', 'preparing'])

/** 管线任务（后端 /api/pipelines 尚未实装，类型从宽） */
interface PipelineTask {
  id?: string
  name?: string
  status?: string
}

function isActive(m: ModuleResponse): boolean {
  return ACTIVE_STATUSES.has((m.service_status || m.status).toLowerCase())
}

/** 状态徽章：圆点 + 中文标签，过渡态带脉冲动画 */
function StatusBadge({ status }: { status: string }) {
  const meta = statusMeta(status)
  return (
    <Badge variant="outline" className={meta.badge}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.transitional && 'animate-pulse',
        )}
      />
      {meta.label}
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
}: {
  icon: typeof Activity
  title: string
  hint?: string
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border py-10 text-center">
      <Icon className="size-8 text-muted-foreground/50" />
      <p className="text-sm text-muted-foreground">{title}</p>
      {hint && <p className="text-xs text-muted-foreground/70">{hint}</p>}
    </div>
  )
}

export function TasksPage() {
  const [modules, setModules] = useState<ModuleResponse[] | null>(null)
  const [statuses, setStatuses] = useState<
    Record<string, ModuleStatusResponse>
  >({})
  const [pipelines, setPipelines] = useState<PipelineTask[] | null>(null)
  const [error, setError] = useState<string | null>(null)

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

  const refreshPipelines = useCallback(async () => {
    try {
      const resp = await fetch('/api/pipelines')
      if (!resp.ok) {
        setPipelines([])
        return
      }
      const data: unknown = await resp.json()
      setPipelines(Array.isArray(data) ? (data as PipelineTask[]) : [])
    } catch {
      setPipelines([])
    }
  }, [])

  useEffect(() => {
    void refresh()
    void refreshPipelines()
    const timer = setInterval(() => {
      if (document.hidden) return
      void refresh()
      void refreshPipelines()
    }, POLL_INTERVAL)
    return () => clearInterval(timer)
  }, [refresh, refreshPipelines])

  const activeModules = (modules ?? []).filter(isActive)
  const runningCount = (modules ?? []).filter(
    (m) => (m.service_status || m.status).toLowerCase() === 'running',
  ).length

  return (
    <PageContainer
      title="任务中心"
      description="运行中的服务、模块状态与管线任务一览"
      actions={
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            void refresh()
            void refreshPipelines()
          }}
        >
          <RefreshCw className="size-3.5" />
          刷新
        </Button>
      }
    >
      <div className="space-y-8">
        {error && (
          <div className="flex items-center gap-2 rounded-lg border border-status-error/30 bg-status-error/10 px-4 py-3 text-sm text-status-error">
            <TriangleAlert className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">加载失败：{error}</span>
            <Button variant="ghost" size="xs" onClick={() => void refresh()}>
              重试
            </Button>
          </div>
        )}

        {/* ── 概览 ── */}
        <div className="flex items-center gap-8 rounded-lg border border-border bg-card px-6 py-4">
          <div>
            <div className="font-mono text-3xl font-bold text-status-running">
              {modules === null ? '–' : runningCount}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              运行中服务
            </div>
          </div>
          <div className="h-8 w-px bg-border" />
          <div>
            <div className="font-mono text-3xl font-bold">
              {modules === null ? '–' : modules.length}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              全部模块
            </div>
          </div>
          <div className="h-8 w-px bg-border" />
          <div>
            <div className="font-mono text-3xl font-bold">
              {pipelines === null ? '–' : pipelines.length}
            </div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              管线任务
            </div>
          </div>
        </div>

        {/* ── 运行中服务 ── */}
        <section>
          <SectionHeader
            icon={Activity}
            title="运行中服务"
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
              title="当前没有运行中的服务"
              hint="前往「模块」页面启动服务后，将在此处实时显示"
            />
          ) : (
            <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {activeModules.map((m) => {
                const meta = statusMeta(m.service_status || m.status)
                const st = statuses[m.id]
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
                        {categoryLabel(m.category)}
                      </Badge>
                    </div>
                    <div className="mt-3 flex items-center gap-4 font-mono text-xs text-muted-foreground">
                      {st?.port != null && <span>端口 {st.port}</span>}
                      {st != null && st.uptime_secs > 0 && (
                        <span>已运行 {formatUptime(st.uptime_secs)}</span>
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
            title="全部模块"
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
              title="暂无已安装模块"
              hint="将模块放入 modules 目录并重启后即可在此管理"
            />
          ) : (
            <div className="overflow-hidden rounded-lg border border-border">
              <Table>
                <TableHeader>
                  <TableRow className="hover:bg-transparent">
                    <TableHead>名称</TableHead>
                    <TableHead>分类</TableHead>
                    <TableHead>版本</TableHead>
                    <TableHead>状态</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {modules.map((m) => (
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
                        {categoryLabel(m.category)}
                      </TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">
                        {m.version}
                      </TableCell>
                      <TableCell>
                        <StatusBadge status={m.service_status || m.status} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </section>

        {/* ── 管线任务 ── */}
        <section>
          <SectionHeader
            icon={GitBranch}
            title="管线任务"
            count={pipelines?.length}
          />
          {pipelines === null ? (
            <div className="space-y-2">
              {Array.from({ length: 2 }).map((_, i) => (
                <Skeleton key={i} className="h-12 rounded-lg" />
              ))}
            </div>
          ) : pipelines.length === 0 ? (
            <EmptyState
              icon={GitBranch}
              title="暂无管线任务"
              hint="在「管线」页面编排并运行管线后，任务将在此处显示"
            />
          ) : (
            <div className="overflow-hidden rounded-lg border border-border">
              {pipelines.map((p, i) => (
                <div
                  key={p.id ?? i}
                  className={cn(
                    'flex items-center justify-between gap-4 px-4 py-3 transition-colors hover:bg-muted/50',
                    i > 0 && 'border-t border-border',
                  )}
                >
                  <span className="truncate text-sm font-medium">
                    {p.name ?? p.id ?? '未命名任务'}
                  </span>
                  <StatusBadge status={p.status ?? 'unknown'} />
                </div>
              ))}
            </div>
          )}
        </section>
      </div>
    </PageContainer>
  )
}
