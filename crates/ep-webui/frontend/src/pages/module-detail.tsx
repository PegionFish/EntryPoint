import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import {
  ChevronLeft,
  CircleAlert,
  Copy,
  Database,
  HardDrive,
  Loader2,
  Play,
  RotateCw,
  Square,
  TerminalSquare,
  TriangleAlert,
} from 'lucide-react'
import { PageContainer } from '@/components/layout/page-container'
import { ConfirmDialog } from '@/components/shared/confirm-dialog'
import { LogViewer } from '@/components/shared/log-viewer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useModuleDetail } from '@/hooks/use-module-detail'
import { useWsState } from '@/hooks/use-ws-state'
import { categoryLabel, statusMeta } from '@/lib/constants'
import { cn, formatBytes, formatUptime } from '@/lib/utils'

/** 归一化状态键（与 statusMeta 保持一致） */
function normalizeStatus(raw: string | null | undefined): string {
  if (!raw) return ''
  return raw
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '_')
    .replace('notready', 'not_ready')
}

function InfoItem({
  label,
  value,
  mono,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd
        className={cn('mt-1 truncate text-sm font-medium', mono && 'font-mono')}
        title={value}
      >
        {value}
      </dd>
    </div>
  )
}

function DetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-8 w-72" />
        <Skeleton className="h-4 w-96 max-w-full" />
      </div>
      <Skeleton className="h-44 rounded-xl" />
      <Skeleton className="h-72 rounded-xl" />
    </div>
  )
}

/** 长路径旁的复制按钮：写入剪贴板并给出 toast 反馈 */
function CopyPathButton({ value, label }: { value: string; label: string }) {
  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(value)
      toast.success(`${label}已复制`)
    } catch {
      toast.error('复制失败，请手动选择文本复制')
    }
  }
  return (
    <Button
      variant="ghost"
      size="xs"
      className="shrink-0 text-muted-foreground/70 hover:text-foreground"
      onClick={() => void handleCopy()}
      title={`复制${label}`}
      aria-label={`复制${label}`}
    >
      <Copy />
    </Button>
  )
}

export default function ModuleDetailPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const wsState = useWsState()
  const {
    module: mod,
    status,
    logs,
    models,
    modelsLoading,
    loading,
    error,
    acting,
    startModule,
    stopModule,
    clearLogs,
  } = useModuleDetail(id)

  const [stopConfirmOpen, setStopConfirmOpen] = useState(false)

  const rawStatus = status?.status ?? mod?.service_status ?? 'stopped'
  const statusKey = normalizeStatus(rawStatus)
  const meta = statusMeta(rawStatus)
  const name = mod?.name ?? id ?? '未知模块'

  const failMsg = (e: unknown) =>
    e instanceof Error ? e.message : String(e)

  const handleStart = async () => {
    try {
      await startModule()
      toast.success(`「${name}」已开始启动`)
    } catch (e) {
      toast.error('启动失败', { description: failMsg(e) })
    }
  }

  /** 确认对话框内执行停止；失败时抛错让对话框保持打开以便重试 */
  const handleStopConfirmed = async () => {
    try {
      await stopModule()
      toast.success(`「${name}」已停止`)
    } catch (e) {
      toast.error('停止失败', { description: failMsg(e) })
      throw e
    }
  }

  const handleRestart = async () => {
    try {
      // 先停止再启动；停止失败（如进程已退出）不阻断启动
      await stopModule().catch(() => undefined)
      await startModule()
      toast.success(`「${name}」已开始重启`)
    } catch (e) {
      toast.error('重启失败', { description: failMsg(e) })
    }
  }

  /** 依据当前状态渲染操作按钮 */
  const renderAction = () => {
    if (acting) {
      return (
        <Button size="sm" disabled>
          <Loader2 className="animate-spin" />
          处理中…
        </Button>
      )
    }
    switch (statusKey) {
      case 'stopped':
        return (
          <Button
            variant="outline"
            size="sm"
            onClick={() => void handleStart()}
            className="border-status-running/50 text-status-running hover:bg-status-running/10 hover:text-status-running dark:bg-transparent dark:hover:bg-status-running/10"
          >
            <Play />
            启动
          </Button>
        )
      case 'running':
      case 'starting':
      case 'preparing':
        return (
          <Button
            variant="destructive"
            size="sm"
            onClick={() => setStopConfirmOpen(true)}
          >
            <Square />
            停止
          </Button>
        )
      case 'error':
        return (
          <Button size="sm" onClick={() => void handleRestart()}>
            <RotateCw />
            重启
          </Button>
        )
      default:
        // not_ready / 未知状态：不展示操作按钮，下方给出提示
        return null
    }
  }

  return (
    <>
      <PageContainer
        title="模块详情"
        description="模块运行状态、操作与实时日志"
        actions={
          <>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/modules')}
            >
              <ChevronLeft />
              返回
            </Button>
            {!loading && renderAction()}
          </>
        }
      >
        <style>{`@keyframes ep-fade-up{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:translateY(0)}}`}</style>

        {loading ? (
          <DetailSkeleton />
        ) : !mod && error ? (
          <div className="flex flex-col items-center justify-center rounded-lg border border-dashed py-16 text-center">
            <CircleAlert className="size-8 text-status-error" />
            <p className="mt-3 text-sm font-medium">模块不存在或加载失败</p>
            <p className="mt-1 max-w-sm break-all px-6 text-xs text-muted-foreground">
              {error}
            </p>
            <Button
              variant="outline"
              size="sm"
              className="mt-4"
              onClick={() => navigate('/modules')}
            >
              <ChevronLeft />
              返回模块列表
            </Button>
          </div>
        ) : (
          <div className="space-y-6">
            {/* 头部：状态徽章 + 名称 + 版本 */}
            <header className="animate-[ep-fade-up_0.35s_ease_both]">
              <div className="flex flex-wrap items-center gap-3">
                <Badge
                  variant="outline"
                  className={cn('gap-1.5 px-2.5 py-1 text-sm', meta.badge)}
                >
                  <span
                    className={cn(
                      'size-2 rounded-full',
                      meta.dot,
                      (statusKey === 'running' || meta.transitional) &&
                        'animate-pulse',
                    )}
                    aria-hidden
                  />
                  {meta.label}
                </Badge>
                <h2 className="text-lg font-semibold tracking-tight">
                  {name}
                </h2>
                {mod?.version && (
                  <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                    v{mod.version}
                  </code>
                )}
                {mod?.category && (
                  <Badge variant="secondary">
                    {categoryLabel(mod.category)}
                  </Badge>
                )}
              </div>
              {mod?.description && (
                <p className="mt-2.5 max-w-2xl text-sm leading-relaxed text-muted-foreground">
                  {mod.description}
                </p>
              )}
              {mod?.path && (
                <p className="mt-1.5 flex items-center gap-1 font-mono text-xs text-muted-foreground/60">
                  <span className="min-w-0 break-all">{mod.path}</span>
                  <CopyPathButton value={mod.path} label="模块路径" />
                </p>
              )}
            </header>

            {/* 未就绪提示：指明具体原因（缺模型/依赖），并给出两个处理入口 */}
            {statusKey === 'not_ready' && (
              <div
                className="flex animate-[ep-fade-up_0.35s_ease_both] items-start gap-3 rounded-lg border border-status-preparing/40 bg-status-preparing/10 px-4 py-3"
                role="alert"
              >
                <TriangleAlert className="mt-0.5 size-4 shrink-0 text-status-preparing" />
                <div className="min-w-0 flex-1 text-sm">
                  <p className="font-medium text-status-preparing">
                    模块未就绪：缺少模型或依赖
                  </p>
                  <p className="mt-0.5 leading-relaxed text-status-preparing/75">
                    该模块尚不满足启动条件——所需的模型文件可能未下载，或系统依赖（如
                    FFmpeg、PyTorch CUDA）缺失。请确认以下两处就绪后再尝试启动。
                  </p>
                  <div className="mt-2.5 flex flex-wrap gap-2">
                    <Button
                      asChild
                      variant="outline"
                      size="xs"
                      className="border-status-preparing/40 bg-transparent text-status-preparing hover:bg-status-preparing/10 hover:text-status-preparing"
                    >
                      <Link to="/models">
                        <Database />
                        前往模型管理
                      </Link>
                    </Button>
                    <Button
                      asChild
                      variant="outline"
                      size="xs"
                      className="border-status-preparing/40 bg-transparent text-status-preparing hover:bg-status-preparing/10 hover:text-status-preparing"
                    >
                      <Link to="/">
                        <HardDrive />
                        查看依赖报告
                      </Link>
                    </Button>
                  </div>
                </div>
              </div>
            )}

            {/* 运行信息 */}
            <Card className="animate-[ep-fade-up_0.35s_ease_both] [animation-delay:60ms]">
              <CardHeader>
                <CardTitle>运行信息</CardTitle>
              </CardHeader>
              <CardContent>
                <dl className="grid grid-cols-2 gap-x-6 gap-y-5 md:grid-cols-4">
                  <InfoItem label="ID" value={mod?.id ?? id ?? '—'} mono />
                  <InfoItem
                    label="版本"
                    value={mod?.version ? `v${mod.version}` : '—'}
                    mono={Boolean(mod?.version)}
                  />
                  <InfoItem
                    label="类别"
                    value={
                      mod?.category ? categoryLabel(mod.category) : '—'
                    }
                  />
                  <div className="min-w-0">
                    <dt className="text-xs text-muted-foreground">状态</dt>
                    <dd className="mt-1">
                      <Badge
                        variant="outline"
                        className={cn('gap-1.5', meta.badge)}
                      >
                        <span
                          className={cn(
                            'size-1.5 rounded-full',
                            meta.dot,
                            (statusKey === 'running' || meta.transitional) &&
                              'animate-pulse',
                          )}
                          aria-hidden
                        />
                        {meta.label}
                      </Badge>
                    </dd>
                  </div>
                  {/* API 无模块 → 设备映射，明示「暂不支持」而非占位符（与仪表盘一致） */}
                  <InfoItem label="设备" value="暂不支持" />
                  <InfoItem
                    label="端口"
                    value={status?.port != null ? String(status.port) : '—'}
                    mono={status?.port != null}
                  />
                  <InfoItem
                    label="运行时长"
                    value={
                      status && status.uptime_secs > 0
                        ? formatUptime(status.uptime_secs)
                        : '—'
                    }
                    mono
                  />
                </dl>
              </CardContent>
            </Card>

            {/* 实时日志 */}
            <Card className="animate-[ep-fade-up_0.35s_ease_both] [animation-delay:120ms]">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <TerminalSquare className="size-4 text-muted-foreground" />
                  实时日志
                </CardTitle>
                <CardAction>
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <span
                      className={cn(
                        'size-1.5 rounded-full',
                        wsState === 'connected'
                          ? 'bg-status-running'
                          : wsState === 'connecting' ||
                              wsState === 'reconnecting'
                            ? 'animate-pulse bg-status-preparing'
                            : 'bg-status-stopped',
                      )}
                      aria-hidden
                    />
                    {wsState === 'connected'
                      ? '实时推送中'
                      : wsState === 'disconnected'
                        ? '推送已断开'
                        : '连接中…'}
                  </span>
                </CardAction>
              </CardHeader>
              <CardContent>
                <LogViewer
                  lines={logs}
                  onClear={clearLogs}
                  exportName={`${id ?? 'module'}-logs.txt`}
                />
                {/* 后端日志缓冲上限 500 行，明示截断行为避免误解（P2-19） */}
                <p className="mt-2 text-xs text-muted-foreground/60">
                  仅保留最近 500 行，实时推送需保持页面连接
                </p>
              </CardContent>
            </Card>

            {/* 模型状态 */}
            <Card className="animate-[ep-fade-up_0.35s_ease_both] [animation-delay:180ms]">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <Database className="size-4 text-muted-foreground" />
                  模型状态
                </CardTitle>
              </CardHeader>
              <CardContent>
                {modelsLoading ? (
                  <div className="space-y-2">
                    <Skeleton className="h-12 rounded-md" />
                    <Skeleton className="h-12 rounded-md" />
                  </div>
                ) : !models || models.models.length === 0 ? (
                  <p className="py-4 text-center text-sm text-muted-foreground">
                    该模块暂无关联模型
                  </p>
                ) : (
                  <div className="space-y-2">
                    {models.models.map((m) => {
                      const mMeta = statusMeta(m.status)
                      return (
                        <div
                          key={m.model_id}
                          className="flex items-center justify-between gap-4 rounded-md border border-border bg-background/60 px-3 py-2.5 transition-colors hover:border-primary/30"
                        >
                          <div className="min-w-0">
                            <p className="truncate text-sm font-medium">
                              {m.name}
                            </p>
                            <p className="flex items-center gap-1 font-mono text-xs text-muted-foreground/70">
                              <span className="truncate" title={m.target_dir}>
                                {m.target_dir}
                              </span>
                              <CopyPathButton
                                value={m.target_dir}
                                label="模型路径"
                              />
                            </p>
                          </div>
                          <div className="flex shrink-0 items-center gap-4">
                            <span className="hidden font-mono text-xs text-muted-foreground sm:inline">
                              {formatBytes(m.size_bytes)}
                            </span>
                            <span className="hidden font-mono text-xs text-muted-foreground sm:inline">
                              {m.file_count != null
                                ? `${m.file_count} 文件`
                                : '—'}
                            </span>
                            <Badge
                              variant="outline"
                              className={cn('gap-1.5', mMeta.badge)}
                            >
                              <span
                                className={cn(
                                  'size-1.5 rounded-full',
                                  mMeta.dot,
                                )}
                                aria-hidden
                              />
                              {mMeta.label}
                            </Badge>
                          </div>
                        </div>
                      )
                    })}
                  </div>
                )}
              </CardContent>
            </Card>
          </div>
        )}
      </PageContainer>

      {/* 停止模块确认（destructive，异步期间保持打开，失败可重试） */}
      <ConfirmDialog
        open={stopConfirmOpen}
        onOpenChange={setStopConfirmOpen}
        title={`停止「${name}」`}
        description="停止后该模块的服务进程将被终止，正在处理的请求会中断，确定要停止吗？"
        confirmLabel="停止模块"
        variant="destructive"
        onConfirm={() => handleStopConfirmed()}
      />
    </>
  )
}
