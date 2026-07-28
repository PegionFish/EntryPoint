import {
  CircleAlert,
  CircleCheck,
  Cpu,
  HardDrive,
  Puzzle,
  TriangleAlert,
} from 'lucide-react'
import type { ReactNode } from 'react'
import type {
  DepReport,
  DeviceResponse,
  ModuleResponse,
  ModuleStatusResponse,
} from '@/api/types'
import { PageContainer } from '@/components/layout/page-container'
import { DeviceCard } from '@/components/shared/device-card'
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

/** 局部状态徽章：状态圆点 + 中文标签，过渡态附加脉冲动画 */
function StatusBadge({ status }: { status: string }) {
  const meta = statusMeta(status)
  return (
    <Badge variant="outline" className={cn('gap-1.5', meta.badge)}>
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

function DeviceCardSkeleton() {
  return (
    <Card className="gap-0 py-0">
      <div className="flex items-center gap-3 border-b border-border px-5 py-4">
        <Skeleton className="size-9 rounded-lg" />
        <div className="flex-1 space-y-1.5">
          <Skeleton className="h-3.5 w-24" />
          <Skeleton className="h-3 w-16" />
        </div>
        <Skeleton className="h-5 w-14 rounded-full" />
      </div>
      <div className="space-y-4 px-5 py-4">
        <div className="space-y-2">
          <div className="flex justify-between">
            <Skeleton className="h-3 w-10" />
            <Skeleton className="h-4 w-12" />
          </div>
          <Skeleton className="h-1.5 w-full rounded-full" />
          <Skeleton className="h-3 w-32" />
        </div>
        <Skeleton className="h-12 w-full rounded-lg" />
      </div>
    </Card>
  )
}

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
  return (
    <section>
      <SectionHeader
        icon={<Cpu className="size-4" />}
        title="计算设备"
        extra={
          devices && devices.length > 0 ? (
            <span className="text-xs text-muted-foreground">
              共{' '}
              <span className="font-mono font-semibold text-foreground">
                {devices.length}
              </span>{' '}
              台
            </span>
          ) : undefined
        }
      />
      {loading && !devices ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <DeviceCardSkeleton />
          <DeviceCardSkeleton />
          <DeviceCardSkeleton />
        </div>
      ) : !devices || devices.length === 0 ? (
        <Card className="border-dashed py-0">
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
                {hasError ? '设备信息加载失败' : '未检测到计算设备'}
              </p>
              <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-muted-foreground">
                {hasError
                  ? '无法获取设备数据，请检查后端服务是否正常运行。'
                  : '未检测到可用的 GPU 或 CPU 计算后端，请确认驱动已正确安装。'}
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

function ModulesSection({
  modules,
  moduleStatus,
  loading,
}: {
  modules: ModuleResponse[] | null
  moduleStatus: Record<string, ModuleStatusResponse>
  loading: boolean
}) {
  const runningCount = modules?.filter(isRunning).length ?? 0

  return (
    <section>
      <SectionHeader
        icon={<Puzzle className="size-4" />}
        title="模块状态"
        extra={
          modules ? (
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <span className="size-1.5 rounded-full bg-status-running" />
              运行中{' '}
              <span className="font-mono font-semibold text-foreground">
                {runningCount}
              </span>
              <span>/</span>
              <span className="font-mono">{modules.length}</span>
            </span>
          ) : undefined
        }
      />
      <Card className="gap-0 overflow-hidden py-0">
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className="text-xs font-medium text-muted-foreground">
                名称
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                类别
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                状态
              </TableHead>
              <TableHead className="text-xs font-medium text-muted-foreground">
                设备
              </TableHead>
              <TableHead className="text-right text-xs font-medium text-muted-foreground">
                端口
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
                <TableCell colSpan={5} className="h-28 text-center">
                  <Puzzle className="mx-auto size-5 text-muted-foreground/60" />
                  <p className="mt-2 text-sm text-muted-foreground">
                    暂无已注册模块
                  </p>
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
                    {/* 当前 API 未提供模块 → 设备映射，暂以占位符显示 */}
                    <TableCell className="text-muted-foreground/70">—</TableCell>
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

/* ---------- 区块三：系统依赖 ---------- */

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
  return (
    <Card
      className={cn(
        'gap-0 py-0 transition-colors',
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
              {available ? '已就绪' : '未安装'}
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
              {guidance ?? '该依赖缺失，部分功能可能无法使用。'}
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
  return (
    <section>
      <SectionHeader icon={<HardDrive className="size-4" />} title="系统依赖" />
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
              available={t.available}
              detail={
                t.available && t.cuda_version ? `CUDA ${t.cuda_version}` : null
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
  const { devices, modules, deps, moduleStatus, loading, error } = useDevices()

  return (
    <PageContainer
      title="仪表盘"
      description="系统总览、设备与模块实时状态"
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
          {error ? '连接异常' : '每 3 秒自动刷新'}
        </div>
      }
    >
      <div className="space-y-8">
        {error && (
          <div className="flex items-start gap-2.5 rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-3">
            <CircleAlert className="mt-0.5 size-4 shrink-0 text-status-error" />
            <div className="min-w-0">
              <p className="text-sm font-medium text-status-error">
                数据加载失败
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
        <DepsSection deps={deps} loading={loading} />
      </div>
    </PageContainer>
  )
}
