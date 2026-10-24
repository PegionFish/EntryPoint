import {
  CircleAlert,
  CircleCheck,
  Cpu,
  HardDrive,
  Puzzle,
  TriangleAlert,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { useEffect, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { api } from '@/api/client'
import type {
  DepReport,
  DeviceResponse,
  ModuleResponse,
  ModuleStatusResponse,
  StorageReport,
} from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { DeviceCard } from '@/components/shared/device-card'
import { NoModulesState } from '@/components/shared/empty-state'
import { CardSkeleton } from '@/components/shared/loading-skeleton'
import { StatStrip } from '@/components/shared/stat-strip'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useDevices } from '@/hooks/use-devices'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn } from '@/lib/utils'

/* ---------- 页面内局部组件 ---------- */

/** 局部状态徽章：状态圆点 + 状态标签，过渡态附加脉冲动画。
 *  四态色对齐 §1 主张 5（令牌 W0 已重定值），运行态圆点附 status-glow-running 辉光（§3.4） */
function StatusBadge({ status }: { status: string }) {
  const meta = statusMeta(status)
  const running = status.trim().toLowerCase() === 'running'
  return (
    <Badge variant="outline" className={cn('gap-1.5', meta.badge)}>
      <span
        className={cn(
          'size-1.5 rounded-full',
          meta.dot,
          meta.transitional && 'animate-pulse',
          running && 'glow-status-running',
        )}
      />
      {meta.label}
    </Badge>
  )
}

/** 区块标题：图标 + H2 + 右侧附加信息 */
function SectionHeader({
  icon,
  title,
  extra,
}: {
  icon: ReactNode
  title: string
  extra?: ReactNode
}) {
  return (
    <div className="mb-3 flex items-center justify-between gap-2">
      <h2 className="flex items-center gap-2 text-base font-semibold">
        <span className="text-primary">{icon}</span>
        {title}
      </h2>
      {extra}
    </div>
  )
}

/* ---------- 骨架屏 ---------- */
/* 设备卡片骨架复用 shared/loading-skeleton 的 CardSkeleton；
   ModuleRowSkeleton / DepCardSkeleton 因分别绑定真实 Table 行结构与
   依赖卡片的图标布局，shared 预设无法直接替换，保留手写。 */

function ModuleRowSkeleton() {
  return (
    <TableRow>
      <TableCell>
        <Skeleton className="h-4 w-32" />
      </TableCell>
      <TableCell>
        <Skeleton className="h-4 w-16" />
      </TableCell>
      <TableCell>
        <Skeleton className="h-5 w-16 rounded-full" />
      </TableCell>
      <TableCell>
        <Skeleton className="h-4 w-8" />
      </TableCell>
      <TableCell className="text-right">
        <Skeleton className="ml-auto h-4 w-10" />
      </TableCell>
    </TableRow>
  )
}

function DepCardSkeleton() {
  return (
    <Card className="gap-0 py-0">
      <CardContent className="flex items-start gap-3 px-5 py-4">
        <Skeleton className="size-9 rounded-lg" />
        <div className="flex-1 space-y-2">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-3 w-56" />
        </div>
      </CardContent>
    </Card>
  )
}

/* ---------- 区块一：计算设备 ---------- */

function DevicesSection({
  devices,
  loading,
  hasError,
}: {
  devices: DeviceResponse[] | null
  loading: boolean
  hasError: boolean
}) {
  const { t } = useTranslation('dashboard')
  return (
    <section>
      <SectionHeader
        icon={<Cpu className="size-4" />}
        title={t('device.title')}
        extra={
          devices && devices.length > 0 ? (
            <span className="text-xs text-muted-foreground">
              <Trans
                i18nKey="device.count"
                ns="dashboard"
                values={{ count: devices.length }}
                components={{
                  b: (
                    <span className="font-mono font-semibold text-foreground" />
                  ),
                }}
              />
            </span>
          ) : undefined
        }
      />
      {loading && !devices ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <CardSkeleton />
          <CardSkeleton />
          <CardSkeleton />
        </div>
      ) : !devices || devices.length === 0 ? (
        <Card className="glass-card border-dashed py-0">
          <CardContent className="flex flex-col items-center justify-center gap-3 px-6 py-14 text-center">
            <div className="flex size-12 items-center justify-center rounded-full bg-muted">
              {hasError ? (
                <CircleAlert className="size-5 text-status-error" />
              ) : (
                <Cpu className="size-5 text-muted-foreground" />
              )}
            </div>
            <div>
              <p className="text-sm font-medium">
                {hasError ? t('device.loadFailed') : t('device.empty')}
              </p>
              <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-muted-foreground">
                {hasError
                  ? t('device.loadFailedHint')
                  : t('device.emptyHint')}
              </p>
            </div>
          </CardContent>
        </Card>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {devices.map((d) => (
            <DeviceCard key={d.id} device={d} />
          ))}
        </div>
      )}
    </section>
  )
}

/* ---------- 区块二：模块状态 ---------- */

function isRunning(m: ModuleResponse): boolean {
  return (m.service_status || m.status).trim().toLowerCase() === 'running'
}

/** 后端规范状态值为小写串，error 表示模块异常 */
function isError(m: ModuleResponse): boolean {
  return (m.service_status || m.status).trim().toLowerCase() === 'error'
}

function ModulesSection({
  modules,
  moduleStatus,
  loading,
}: {
  modules: ModuleResponse[] | null
  moduleStatus: Record<string, ModuleStatusResponse>
  loading: boolean
}) {
  const { t } = useTranslation('dashboard')
  const runningCount = modules?.filter(isRunning).length ?? 0
  const errorCount = modules?.filter(isError).length ?? 0

  return (
    <section>
      <SectionHeader
        icon={<Puzzle className="size-4" />}
        title={t('module.title')}
        extra={
          modules ? (
            <span className="flex items-center gap-3 text-xs text-muted-foreground">
              <span className="flex items-center gap-1.5">
                <span className="size-1.5 rounded-full bg-status-running" />
                {t('common:status.running')}{' '}
                <span className="font-mono font-semibold text-foreground">
                  {runningCount}
                </span>
                <span>/</span>
                <span className="font-mono">{modules.length}</span>
              </span>
              {/* 异常模块计数：>0 时红色强调，为 0 时保持正常色（P2-38） */}
              <span className="flex items-center gap-1.5">
                <CircleAlert
                  className={cn(
                    'size-3.5',
                    errorCount > 0
                      ? 'text-status-error'
                      : 'text-muted-foreground/50',
                  )}
                />
                {t('module.errorLabel')}{' '}
                <span
                  className={cn(
                    'font-mono font-semibold',
                    errorCount > 0 ? 'text-status-error' : 'text-foreground',
                  )}
                >
                  {errorCount}
                </span>
              </span>
            </span>
          ) : undefined
        }
      />
      <Card className="glass-card gap-0 overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className="text-xs font-medium text-muted-foreground">
                {t('common:label.name')}
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                {t('table.header.category')}
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                {t('common:label.status')}
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                {t('table.header.device')}
              </TableHead>
              <TableHead className="text-right text-xs font-medium text-muted-foreground">
                {t('table.header.port')}
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {loading && !modules ? (
              Array.from({ length: 3 }).map((_, i) => (
                <ModuleRowSkeleton key={i} />
              ))
            ) : !modules || modules.length === 0 ? (
              <TableRow>
                <TableCell colSpan={5} className="p-0">
                  {/* 复用 shared/empty-state 预设组件，替代手写空态 */}
                  <NoModulesState
                    className="py-8"
                    description={t('module.emptyHint')}
                  />
                </TableCell>
              </TableRow>
            ) : (
              modules.map((m) => {
                const st = moduleStatus[m.id]
                return (
                  <TableRow key={m.id}>
                    <TableCell>
                      <p className="font-medium leading-tight">{m.name}</p>
                      <p className="mt-0.5 font-mono text-xs text-muted-foreground">
                        v{m.version}
                      </p>
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant="outline"
                        className="border-border bg-muted/60 font-normal text-muted-foreground"
                      >
                        {categoryLabel(m.category)}
                      </Badge>
                    </TableCell>
                    <TableCell>
                      <StatusBadge status={m.service_status || m.status} />
                    </TableCell>
                    {/* API 无模块 → 设备映射，无法给出真实设备归属；
                        明示「暂不支持」而非占用符号，避免误以为数据缺失（P2-37） */}
                    <TableCell className="text-xs text-muted-foreground/60">
                      {t('table.deviceUnsupported')}
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">
                      {st?.port != null ? (
                        <span className="text-foreground">{st.port}</span>
                      ) : (
                        <span className="text-muted-foreground/70">—</span>
                      )}
                    </TableCell>
                  </TableRow>
                )
              })
            )}
          </TableBody>
        </Table>
      </Card>
    </section>
  )
}

/* ---------- 区块三：存储水位（T-11） ---------- */

/** 字节数 → 人类可读（1024 进制：GiB 保留 1 位小数，MiB/KiB 取整） */
function formatBytes(bytes: number): string {
  const gib = bytes / 1024 ** 3
  if (gib >= 1) return `${gib.toFixed(1)} GiB`
  const mib = bytes / 1024 ** 2
  if (mib >= 1) return `${mib.toFixed(0)} MiB`
  return `${Math.round(bytes / 1024)} KiB`
}

/** 存储水位卡片：workspace / models / tasks 三目录已用 + 模型缓存盘剩余。
 *
 *  数据来自 GET /api/storage（T-11），30s 轮询一次；失败时保留上一次数据
 *  只是翻转失败提示，避免网络抖动把整张卡片清空。
 *  workspace 的 used_bytes 已包含其下的 tasks，聚合总占用时只取
 *  workspace + models，避免 tasks 重复计数。
 *  占用条按「(workspace + models) /（已用 + 模型盘剩余）」估算：默认部署三者
 *  同盘，自定义 cache_dir 跨盘时该比例仅供参考（标题旁同时给出精确剩余值）。
 *  free_bytes 为 null（探测失败）时比例不可知，显示中性态而不是 100%。 */
function StorageSection() {
  const { t } = useTranslation('dashboard')
  const [report, setReport] = useState<StorageReport | null>(null)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    let cancelled = false
    const load = () => {
      api
        .storage()
        .then((data) => {
          if (cancelled) return
          setReport(data)
          setFailed(false)
        })
        .catch(() => {
          if (!cancelled) setFailed(true)
        })
    }
    load()
    const timer = window.setInterval(load, 30_000)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [])

  const freeBytes = report?.free_bytes ?? null
  // workspace 已包含 tasks：只聚合两个不重叠目录，避免重复计数。
  const usedTotal = report
    ? (report.dirs.find((dir) => dir.name === 'workspace')?.used_bytes ?? 0) +
      (report.dirs.find((dir) => dir.name === 'models')?.used_bytes ?? 0)
    : 0
  // 探测失败时比例不可知：capacity/ratio 置 null，走中性态。
  const capacity = freeBytes == null ? null : usedTotal + freeBytes
  const ratio =
    capacity != null && capacity > 0 ? usedTotal / capacity : null
  const tone =
    ratio == null
      ? 'bg-muted'
      : ratio >= 0.9
        ? 'bg-status-error'
        : ratio >= 0.7
          ? 'bg-status-preparing'
          : 'bg-status-running'

  return (
    <section>
      <SectionHeader
        icon={<HardDrive className="size-4" />}
        title={t('storage.title')}
        extra={
          report ? (
            <span className="text-xs text-muted-foreground">
              <Trans
                i18nKey="storage.summary"
                ns="dashboard"
                values={{
                  used: formatBytes(usedTotal),
                  free: freeBytes == null ? '—' : formatBytes(freeBytes),
                }}
                components={{
                  b: <span className="font-mono font-semibold text-foreground" />,
                }}
              />
            </span>
          ) : undefined
        }
      />
      {report == null ? (
        failed ? (
          <Card className="glass-card border-dashed py-0">
            <CardContent className="flex items-start gap-2.5 px-5 py-4">
              <CircleAlert className="mt-0.5 size-4 shrink-0 text-status-error" />
              <div>
                <p className="text-sm font-medium text-status-error">
                  {t('storage.loadFailed')}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t('storage.loadFailedHint')}
                </p>
              </div>
            </CardContent>
          </Card>
        ) : (
          <Card className="glass-card py-0">
            <CardContent className="space-y-3 px-5 py-4">
              {Array.from({ length: 3 }).map((_, i) => (
                <Skeleton key={i} className="h-5 w-full" />
              ))}
            </CardContent>
          </Card>
        )
      ) : (
        <Card className="glass-card py-0">
          <CardContent className="space-y-4 px-5 py-4">
            {/* 总占用条：>=70% 预警、>=90% 危险（状态色令牌） */}
            <div className="space-y-1.5">
              <div className="flex items-baseline justify-between text-xs">
                <span className="text-muted-foreground">
                  {t('storage.usage')}
                </span>
                <span className="font-mono text-muted-foreground">
                  {ratio == null
                    ? t('storage.unknown')
                    : `${(ratio * 100).toFixed(1)}%`}
                </span>
              </div>
              <div
                className="h-1.5 overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-label={t('storage.title')}
                aria-valuenow={
                  ratio == null ? undefined : Math.round(ratio * 100)
                }
                aria-valuetext={ratio == null ? t('storage.unknown') : undefined}
                aria-valuemin={0}
                aria-valuemax={100}
              >
                <div
                  className={cn(
                    'h-full rounded-full transition-[width] duration-500',
                    tone,
                  )}
                  style={{
                    width:
                      ratio == null ? '0%' : `${Math.min(100, ratio * 100)}%`,
                  }}
                />
              </div>
            </div>

            <div className="space-y-2.5">
              {report.dirs.map((dir) => (
                <div
                  key={dir.name}
                  className="flex items-baseline justify-between gap-3 text-xs"
                >
                  <div className="min-w-0">
                    <span className="font-medium">
                      {t(`storage.dir.${dir.name}`)}
                    </span>
                    <p className="truncate font-mono text-[11px] text-muted-foreground/70">
                      {dir.path}
                    </p>
                  </div>
                  <span className="shrink-0 font-mono text-muted-foreground">
                    {formatBytes(dir.used_bytes)}
                  </span>
                </div>
              ))}
            </div>

            {failed && (
              <p className="text-xs text-status-error">
                {t('storage.loadFailedHint')}
              </p>
            )}
          </CardContent>
        </Card>
      )}
    </section>
  )
}

/* ---------- 区块四：系统依赖 ---------- */

function DepCard({
  title,
  tag,
  available,
  detail,
  guidance,
}: {
  title: string
  /** 次级标识（如模块 id） */
  tag?: string
  available: boolean
  /** 就绪时的详情（版本 / 路径） */
  detail?: string | null
  /** 缺失时的安装指引 */
  guidance?: string | null
}) {
  const { t } = useTranslation('dashboard')
  return (
    <Card
      className={cn(
        'glass-card gap-0 py-0 transition-colors',
        !available && 'border-status-preparing/40 bg-status-preparing/5',
      )}
    >
      <CardContent className="flex items-start gap-3 px-5 py-4">
        <div
          className={cn(
            'flex size-9 shrink-0 items-center justify-center rounded-lg',
            available ? 'bg-status-running/15' : 'bg-status-preparing/15',
          )}
        >
          {available ? (
            <CircleCheck className="size-4.5 text-status-running" />
          ) : (
            <TriangleAlert className="size-4.5 text-status-preparing" />
          )}
        </div>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-sm font-semibold">{title}</p>
            {tag && (
              <Badge
                variant="outline"
                className="border-border bg-muted font-mono text-xs font-normal text-muted-foreground"
              >
                {tag}
              </Badge>
            )}
            <Badge
              variant="outline"
              className={
                available
                  ? 'border-status-running/30 bg-status-running/15 text-status-running'
                  : 'border-status-preparing/30 bg-status-preparing/15 text-status-preparing'
              }
            >
              {available ? t('dep.ready') : t('dep.notInstalled')}
            </Badge>
          </div>
          {available ? (
            detail && (
              <p className="truncate font-mono text-xs text-muted-foreground">
                {detail}
              </p>
            )
          ) : (
            <p className="text-xs leading-relaxed text-status-preparing">
              {guidance ?? t('dep.missingHint')}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

function DepsSection({
  deps,
  loading,
}: {
  deps: DepReport | null
  loading: boolean
}) {
  const { t } = useTranslation('dashboard')
  return (
    <section>
      <SectionHeader
        icon={<HardDrive className="size-4" />}
        title={t('dep.title')}
      />
      {loading && !deps ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <DepCardSkeleton />
          <DepCardSkeleton />
        </div>
      ) : !deps ? null : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <DepCard
            title="FFmpeg"
            available={deps.ffmpeg.available}
            detail={
              deps.ffmpeg.available
                ? [deps.ffmpeg.version, deps.ffmpeg.path]
                    .filter(Boolean)
                    .join(' · ')
                : null
            }
            guidance={deps.ffmpeg.guidance}
          />
          {deps.torch_cuda.map((t) => (
            <DepCard
              key={t.module_id}
              title="PyTorch CUDA"
              tag={t.module_id}
              available={t.cuda_available}
              detail={
                t.cuda_available && t.torch_version
                  ? `PyTorch ${t.torch_version}`
                  : null
              }
              guidance={t.guidance}
            />
          ))}
        </div>
      )}
    </section>
  )
}

/* ---------- 页面 ---------- */

export function DashboardPage() {
  const { t } = useTranslation('dashboard')
  const { devices, modules, deps, moduleStatus, loading, error } = useDevices()
  // 统计条带（§6.2-D，与桌面端 IA 对齐：设备 / 模块 / 运行中 / 异常）
  const runningCount = modules?.filter(isRunning).length
  const errorCount = modules?.filter(isError).length

  return (
    <PageContainer
      title={t('page.title')}
      description={t('page.description')}
      actions={
        <div className="flex items-center gap-2 rounded-full border border-border bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
          <span className="relative flex size-2">
            {!error && (
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-status-running opacity-60" />
            )}
            <span
              className={cn(
                'relative inline-flex size-2 rounded-full',
                error ? 'bg-status-error' : 'bg-status-running',
              )}
            />
          </span>
          {error ? t('page.connectionError') : t('page.autoRefresh')}
        </div>
      }
    >
      <div className="space-y-8">
        {/* 统计条带：大号等宽数字 + 全大写灰阶标签（主张 4 数据仪表盘化） */}
        <StatStrip
          items={[
            {
              label: t('stat.devices'),
              value: devices?.length ?? '–',
            },
            {
              label: t('stat.modules'),
              value: modules?.length ?? '–',
            },
            {
              label: t('stat.running'),
              value: runningCount ?? '–',
              tone: 'text-status-running',
            },
            {
              label: t('stat.errors'),
              value: errorCount ?? '–',
              tone:
                errorCount != null && errorCount > 0
                  ? 'text-status-error'
                  : undefined,
            },
          ]}
        />

        {error && (
          <div className="flex items-start gap-2.5 rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-3">
            <CircleAlert className="mt-0.5 size-4 shrink-0 text-status-error" />
            <div className="min-w-0">
              <p className="text-sm font-medium text-status-error">
                {t('page.loadFailed')}
              </p>
              <p className="mt-0.5 break-all font-mono text-xs text-status-error/80">
                {error}
              </p>
            </div>
          </div>
        )}

        <DevicesSection
          devices={devices}
          loading={loading}
          hasError={error != null}
        />
        <ModulesSection
          modules={modules}
          moduleStatus={moduleStatus}
          loading={loading}
        />
        <StorageSection />
        <DepsSection deps={deps} loading={loading} />
      </div>
    </PageContainer>
  )
}
