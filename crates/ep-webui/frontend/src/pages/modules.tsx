import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import {
  Activity,
  CircleAlert,
  CircleStop,
  CircleX,
  Puzzle,
  RefreshCw,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { api } from '@/api/client'
import { PageContainer } from '@/components/layout/page-container'
import { ConfirmDialog } from '@/components/shared/confirm-dialog'
import { NoModulesState } from '@/components/shared/empty-state'
import { CardSkeleton } from '@/components/shared/loading-skeleton'
import { ModuleCard } from '@/components/shared/module-card'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useModules } from '@/hooks/use-modules'
import { CATEGORY_LABELS, categoryLabel } from '@/lib/constants'
import { cn } from '@/lib/utils'
import type { ModuleResponse, ModuleStatusResponse } from '@/api/types'

/** 归一化状态键（与 statusMeta 保持一致） */
function normalizeStatus(raw: string): string {
  return raw
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '_')
    .replace('notready', 'not_ready')
}

/** 优先使用状态轮询结果，缺省时回退列表接口的 service_status */
function effectiveStatus(
  m: ModuleResponse,
  statusMap: Record<string, ModuleStatusResponse>,
): string {
  return statusMap[m.id]?.status ?? m.service_status
}

interface SummaryStatProps {
  icon: LucideIcon
  label: string
  value: number
  /** 数值与图标色调（默认前景色） */
  tone?: string
}

function SummaryStat({ icon: Icon, label, value, tone }: SummaryStatProps) {
  return (
    <div className="flex items-center gap-3.5 bg-card px-5 py-4">
      <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted/70">
        <Icon className={cn('size-4', tone ?? 'text-muted-foreground')} />
      </div>
      <div className="min-w-0">
        <p
          className={cn(
            'text-2xl font-bold leading-none tracking-tight tabular-nums',
            tone,
          )}
        >
          {value}
        </p>
        <p className="mt-1.5 text-xs text-muted-foreground">{label}</p>
      </div>
    </div>
  )
}

export function ModulesPage() {
  const { t } = useTranslation('modules')
  const { modules, statusMap, loading, error, refresh } = useModules()
  const [spinning, setSpinning] = useState(false)
  /** 待确认停止的模块（非 null 时显示确认对话框） */
  const [stopTarget, setStopTarget] = useState<ModuleResponse | null>(null)
  /** 正在启动的模块 id（卡片启动按钮显示进行中状态） */
  const [startingId, setStartingId] = useState<string | null>(null)

  const handleRefresh = async () => {
    setSpinning(true)
    try {
      await refresh()
    } finally {
      setSpinning(false)
    }
  }

  /** 卡片快捷启动已停止的模块；与详情页同一启动 API，成功后刷新同步最新状态 */
  const handleStart = async (m: ModuleResponse) => {
    setStartingId(m.id)
    try {
      const res = await api.startModule(m.id)
      if (res.error) throw new Error(res.error)
      toast.success(t('toast.startStarted', { name: m.name }))
      await refresh()
    } catch (e) {
      toast.error(t('toast.startFailed'), {
        description: e instanceof Error ? e.message : String(e),
      })
    } finally {
      setStartingId((cur) => (cur === m.id ? null : cur))
    }
  }

  /** 确认对话框内执行停止；失败时抛错让对话框保持打开以便重试 */
  const handleStopConfirmed = async () => {
    if (!stopTarget) return
    try {
      const res = await api.stopModule(stopTarget.id)
      if (res.error) throw new Error(res.error)
      toast.success(t('toast.stopSucceeded', { name: stopTarget.name }))
      await refresh()
    } catch (e) {
      toast.error(t('toast.stopFailed'), {
        description: e instanceof Error ? e.message : String(e),
      })
      throw e
    }
  }

  // 按类别分组（已知类别按 CATEGORY_LABELS 顺序，未知类别排在末尾）
  const groups = useMemo(() => {
    const byCat = new Map<string, ModuleResponse[]>()
    for (const m of modules) {
      const cat = (m.category || 'other').toLowerCase()
      const arr = byCat.get(cat)
      if (arr) arr.push(m)
      else byCat.set(cat, [m])
    }
    const known = Object.keys(CATEGORY_LABELS)
    return [...byCat.keys()]
      .sort((a, b) => {
        const ia = known.indexOf(a)
        const ib = known.indexOf(b)
        if (ia !== -1 && ib !== -1) return ia - ib
        if (ia !== -1) return -1
        if (ib !== -1) return 1
        return a.localeCompare(b)
      })
      .map((category) => ({ category, items: byCat.get(category)! }))
  }, [modules])

  const counts = useMemo(() => {
    let running = 0
    let stopped = 0
    let errored = 0
    for (const m of modules) {
      const k = normalizeStatus(effectiveStatus(m, statusMap))
      if (k === 'running') running += 1
      else if (k === 'stopped') stopped += 1
      else if (k === 'error') errored += 1
    }
    return { running, stopped, errored }
  }, [modules, statusMap])

  return (
    <PageContainer
      title={t('page.title')}
      description={t('page.description')}
      actions={
        <Button
          variant="outline"
          size="sm"
          onClick={() => void handleRefresh()}
          disabled={spinning || loading}
        >
          <RefreshCw className={cn(spinning && 'animate-spin')} />
          {t('common:action.refresh')}
        </Button>
      }
    >
      <style>{`@keyframes ep-fade-up{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:translateY(0)}}`}</style>

      {loading ? (
        <div className="space-y-6">
          <Skeleton className="h-[72px] rounded-lg" />
          {/* 复用 shared/loading-skeleton 预设组件，避免各页面手写骨架 */}
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 6 }).map((_, i) => (
              <CardSkeleton key={i} />
            ))}
          </div>
        </div>
      ) : error && modules.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-lg border border-dashed py-16 text-center">
          <CircleX className="size-8 text-status-error" />
          <p className="mt-3 text-sm font-medium">{t('page.loadFailed')}</p>
          <p className="mt-1 max-w-sm break-all px-6 text-xs text-muted-foreground">
            {error}
          </p>
          <Button
            variant="outline"
            size="sm"
            className="mt-4"
            onClick={() => void handleRefresh()}
          >
            <RefreshCw />
            {t('common:action.retry')}
          </Button>
        </div>
      ) : modules.length === 0 ? (
        /* 复用 shared/empty-state 预设组件，替代手写空态 */
        <div className="rounded-lg border border-dashed">
          <NoModulesState
            description={t('page.emptyHint')}
            action={{
              label: t('common:action.refresh'),
              onClick: () => void handleRefresh(),
            }}
          />
        </div>
      ) : (
        <div className="space-y-8">
          {/* 汇总条 */}
          <div
            className="grid animate-[ep-fade-up_0.35s_ease_both] grid-cols-2 gap-px overflow-hidden rounded-lg border border-border bg-border lg:grid-cols-4"
            role="status"
            aria-label={t('summary.ariaLabel')}
          >
            <SummaryStat
              icon={Puzzle}
              label={t('summary.total')}
              value={modules.length}
            />
            <SummaryStat
              icon={Activity}
              label={t('common:status.running')}
              value={counts.running}
              tone="text-status-running"
            />
            <SummaryStat
              icon={CircleStop}
              label={t('common:status.stopped')}
              value={counts.stopped}
              tone="text-muted-foreground"
            />
            <SummaryStat
              icon={CircleAlert}
              label={t('common:status.error')}
              value={counts.errored}
              tone="text-status-error"
            />
          </div>

          {/* 轮询失败提示（仍展示最近一次成功的数据） */}
          {error && (
            <p className="flex items-center gap-2 rounded-md border border-status-error/30 bg-status-error/10 px-3 py-2 text-xs text-status-error">
              <CircleAlert className="size-3.5 shrink-0" />
              {t('poll.failed', { error })}
            </p>
          )}

          {/* 按类别分组 */}
          {groups.map((group, gi) => {
            const runningInGroup = group.items.filter(
              (m) =>
                normalizeStatus(effectiveStatus(m, statusMap)) === 'running',
            ).length
            return (
              <section
                key={group.category}
                className="animate-[ep-fade-up_0.35s_ease_both]"
                style={{ animationDelay: `${(gi + 1) * 60}ms` }}
              >
                <div className="mb-3 flex items-end justify-between gap-3">
                  <h2 className="flex items-center gap-2.5 text-base font-semibold">
                    <span
                      className="h-4 w-1 rounded-full bg-primary/80"
                      aria-hidden
                    />
                    {categoryLabel(group.category)}
                    <span className="text-xs font-normal text-muted-foreground">
                      {t('group.moduleCount', { count: group.items.length })}
                    </span>
                  </h2>
                  <span className="font-mono text-xs text-muted-foreground">
                    <span
                      className={cn(
                        runningInGroup > 0 && 'text-status-running',
                      )}
                    >
                      {runningInGroup}
                    </span>
                    /{group.items.length} {t('group.runningLabel')}
                  </span>
                </div>
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
                  {group.items.map((m) => (
                    <ModuleCard
                      key={m.id}
                      module={m}
                      status={statusMap[m.id]?.status}
                      port={statusMap[m.id]?.port ?? null}
                      starting={startingId === m.id}
                      onStart={() => void handleStart(m)}
                      onStop={() => setStopTarget(m)}
                    />
                  ))}
                </div>
              </section>
            )
          })}
        </div>
      )}

      {/* 停止模块确认（destructive，异步期间保持打开，失败可重试） */}
      <ConfirmDialog
        open={stopTarget !== null}
        onOpenChange={(open) => {
          if (!open) setStopTarget(null)
        }}
        title={t('stopDialog.title', { name: stopTarget?.name ?? '' })}
        description={t('stopDialog.description')}
        confirmLabel={t('stopDialog.confirm')}
        variant="destructive"
        onConfirm={() => handleStopConfirmed()}
      />
    </PageContainer>
  )
}
