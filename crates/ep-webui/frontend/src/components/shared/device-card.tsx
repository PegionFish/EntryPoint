import { Activity, Cpu, MemoryStick, Thermometer, Zap } from 'lucide-react'
import type { DeviceResponse } from '@/api/types'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Progress } from '@/components/ui/progress'
import { cn, formatMB } from '@/lib/utils'

/** 后端 → 徽章 / 图标座配色（cuda 绿、cpu 蓝，其余灰） */
function backendTone(backend: string): string {
  const b = backend.toLowerCase()
  if (b.includes('cuda')) return 'bg-status-running/15 text-status-running'
  if (b.includes('cpu')) return 'bg-status-starting/15 text-status-starting'
  return 'bg-muted text-muted-foreground'
}

/** 后端 → 图标（GPU 闪电、CPU 芯片） */
function BackendIcon({
  backend,
  className,
}: {
  backend: string
  className?: string
}) {
  const Icon = backend.toLowerCase().includes('cuda') ? Zap : Cpu
  return <Icon className={className} />
}

/**
 * 内存占用 → 进度条 / 数值配色。
 * 注意：指示器覆盖类必须写成完整字面量，Tailwind 才不会漏生成。
 * <70% 绿、70–90% 黄、>90% 红。
 */
function memoryLevel(percent: number): { bar: string; text: string } {
  if (percent > 90) {
    return {
      bar: '[&>[data-slot=progress-indicator]]:bg-status-error',
      text: 'text-status-error',
    }
  }
  if (percent >= 70) {
    return {
      bar: '[&>[data-slot=progress-indicator]]:bg-status-preparing',
      text: 'text-status-preparing',
    }
  }
  return {
    bar: '[&>[data-slot=progress-indicator]]:bg-status-running',
    text: 'text-status-running',
  }
}

/** 温度 → 数值配色：≥85°C 红、≥70°C 黄 */
function tempTone(temp: number): string {
  if (temp >= 85) return 'text-status-error'
  if (temp >= 70) return 'text-status-preparing'
  return 'text-foreground'
}

interface DeviceCardProps {
  device: DeviceResponse
}

/** 计算设备卡片：名称、后端徽章、内存进度条、利用率、温度 */
export function DeviceCard({ device }: DeviceCardProps) {
  const tone = backendTone(device.backend)
  const percent =
    device.used_memory_mb != null &&
    device.total_memory_mb != null &&
    device.total_memory_mb > 0
      ? Math.min(100, (device.used_memory_mb / device.total_memory_mb) * 100)
      : null
  const level = percent != null ? memoryLevel(percent) : null

  return (
    <Card className="gap-0 overflow-hidden py-0 transition-all duration-200 hover:-translate-y-0.5 hover:border-primary/40 hover:shadow-lg hover:shadow-primary/5">
      <CardHeader className="flex flex-row items-center gap-3 border-b border-border px-5 py-4">
        <div
          className={cn(
            'flex size-9 shrink-0 items-center justify-center rounded-lg',
            tone,
          )}
        >
          <BackendIcon backend={device.backend} className="size-4.5" />
        </div>
        <div className="min-w-0 flex-1">
          <CardTitle className="truncate text-sm">{device.name}</CardTitle>
          <p className="mt-1 truncate font-mono text-xs text-muted-foreground">
            {device.id}
          </p>
        </div>
        <Badge
          variant="outline"
          className={cn('shrink-0 font-mono uppercase tracking-wide', tone)}
        >
          {device.backend}
        </Badge>
      </CardHeader>

      <CardContent className="space-y-4 px-5 py-4">
        {/* 内存 */}
        <div className="space-y-2">
          <div className="flex items-baseline justify-between gap-2">
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <MemoryStick className="size-3.5" />
              显存
            </span>
            {percent != null && level ? (
              <span
                className={cn(
                  'font-mono text-base font-bold leading-none',
                  level.text,
                )}
              >
                {percent.toFixed(1)}%
              </span>
            ) : (
              <span className="text-xs text-muted-foreground">不可用</span>
            )}
          </div>
          <Progress
            value={percent ?? 0}
            className={cn('h-1.5 bg-muted', level?.bar)}
          />
          <p className="font-mono text-xs text-muted-foreground">
            {formatMB(device.used_memory_mb)} / {formatMB(device.total_memory_mb)}
          </p>
        </div>

        {/* 利用率 / 温度 */}
        <div className="grid grid-cols-2 divide-x divide-border rounded-lg border border-border bg-muted/30">
          <div className="flex items-center gap-2.5 px-3 py-2.5">
            <Activity className="size-3.5 shrink-0 text-muted-foreground" />
            <div className="min-w-0">
              <p className="text-[10px] leading-none text-muted-foreground">
                利用率
              </p>
              <p className="mt-1 font-mono text-sm font-semibold leading-none">
                {device.utilization != null ? `${device.utilization}%` : '—'}
              </p>
            </div>
          </div>
          <div className="flex items-center gap-2.5 px-3 py-2.5">
            <Thermometer className="size-3.5 shrink-0 text-muted-foreground" />
            <div className="min-w-0">
              <p className="text-[10px] leading-none text-muted-foreground">
                温度
              </p>
              <p
                className={cn(
                  'mt-1 font-mono text-sm font-semibold leading-none',
                  device.temperature != null && tempTone(device.temperature),
                )}
              >
                {device.temperature != null
                  ? `${device.temperature}°C`
                  : '—'}
              </p>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
