import { useCallback, useLayoutEffect, useRef, useState } from 'react'
import { ArrowDownToLine, Lock, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface LogViewerProps {
  /** 日志行（按时间顺序） */
  lines: string[]
  /** 清空回调（仅清空前端显示）；不提供时隐藏清空按钮 */
  onClear?: () => void
  /** 滚动区最大高度（px），默认 400 */
  maxHeight?: number
  className?: string
}

/**
 * 终端风格日志查看器。
 *
 * - 等宽字体 + 行号 + 深色背景；
 * - 新日志到达自动滚动到底部（锁定状态），用户向上翻阅时自动解锁，
 *   点击「回到底部」重新锁定；
 * - 「清空」仅清除前端显示，不影响服务端日志缓冲。
 */
export function LogViewer({
  lines,
  onClear,
  maxHeight = 400,
  className,
}: LogViewerProps) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const [locked, setLocked] = useState(true)

  // 锁定状态下，日志更新后始终贴底
  useLayoutEffect(() => {
    const el = scrollRef.current
    if (el && locked) el.scrollTop = el.scrollHeight
  }, [lines, locked])

  // 用户滚动：离开底部即解锁，滚回底部自动重新锁定
  const handleScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 32
    setLocked((prev) => (prev === atBottom ? prev : atBottom))
  }, [])

  const followBottom = () => {
    setLocked(true)
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }

  return (
    <div className={cn('space-y-2', className)}>
      <div className="flex items-center justify-between gap-2">
        <span className="font-mono text-xs text-muted-foreground">
          共 {lines.length} 行
        </span>
        <div className="flex items-center gap-1.5">
          {locked ? (
            <Button
              variant="secondary"
              size="xs"
              onClick={followBottom}
              title="已锁定底部滚动"
            >
              <Lock />
              已跟随
            </Button>
          ) : (
            <Button
              variant="outline"
              size="xs"
              onClick={followBottom}
              className="border-primary/50 text-primary"
              title="有新日志，点击回到底部"
            >
              <ArrowDownToLine />
              回到底部
            </Button>
          )}
          {onClear && (
            <Button
              variant="ghost"
              size="xs"
              onClick={onClear}
              disabled={lines.length === 0}
              title="清空前端显示"
            >
              <Trash2 />
              清空
            </Button>
          )}
        </div>
      </div>

      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="overflow-y-auto rounded-md border border-border bg-background font-mono text-xs leading-5"
        style={{ maxHeight }}
      >
        {lines.length === 0 ? (
          <div className="flex h-24 items-center justify-center text-muted-foreground/60">
            暂无日志
          </div>
        ) : (
          <div className="py-2">
            {lines.map((line, i) => (
              <div key={i} className="flex px-3 transition-colors hover:bg-muted/40">
                <span className="w-10 shrink-0 select-none pr-3 text-right text-muted-foreground/40">
                  {i + 1}
                </span>
                <span className="min-w-0 whitespace-pre-wrap break-all text-foreground/85">
                  {line}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
