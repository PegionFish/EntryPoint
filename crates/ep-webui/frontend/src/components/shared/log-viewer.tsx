import {
  memo,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import type { ReactNode } from 'react'
import {
  ArrowDownToLine,
  Download,
  Lock,
  Search,
  Trash2,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { cn } from '@/lib/utils'

/** 级别过滤选项 */
type LogLevelFilter = 'all' | 'error' | 'warn' | 'info'

/** 从行内识别出的日志级别 */
type DetectedLevel = 'error' | 'warn' | 'info'

const LEVEL_RE = /\b(ERROR|WARN(?:ING)?|INFO)\b/i

/** 识别日志行级别：命中 ERROR / WARN(WARNING) / INFO，无法识别返回 null */
function detectLevel(line: string): DetectedLevel | null {
  const m = LEVEL_RE.exec(line)
  if (!m) return null
  const level = m[1].toUpperCase()
  if (level === 'ERROR') return 'error'
  if (level === 'WARN' || level === 'WARNING') return 'warn'
  return 'info'
}

/** 大小写不敏感地高亮 query 命中片段 */
function highlight(line: string, query: string): ReactNode {
  if (!query) return line
  const lower = line.toLowerCase()
  const idx0 = lower.indexOf(query)
  if (idx0 === -1) return line
  const parts: ReactNode[] = []
  let start = 0
  let idx = idx0
  while (idx !== -1) {
    if (idx > start) parts.push(line.slice(start, idx))
    parts.push(
      <mark
        key={idx}
        className="rounded-[2px] bg-status-preparing/40 text-inherit"
      >
        {line.slice(idx, idx + query.length)}
      </mark>,
    )
    start = idx + query.length
    idx = lower.indexOf(query, start)
  }
  if (start < line.length) parts.push(line.slice(start))
  return parts
}

interface LogRowData {
  line: string
  /** 原始缓冲中的行号（1 起），过滤后保持不变便于对照 */
  num: number
  level: DetectedLevel | null
}

/** 单行渲染（memo 化：1000 行缓冲下仅重渲染受影响的行） */
const LogRow = memo(function LogRow({
  row,
  query,
}: {
  row: LogRowData
  query: string
}) {
  return (
    <div className="flex px-3 hover:bg-muted/40">
      <span className="w-10 shrink-0 select-none pr-3 text-right text-muted-foreground/40">
        {row.num}
      </span>
      <span
        className={cn(
          'min-w-0 whitespace-pre-wrap break-all',
          row.level === 'error'
            ? 'text-status-error'
            : row.level === 'warn'
              ? 'text-status-preparing'
              : 'text-foreground/85',
        )}
      >
        {highlight(row.line, query)}
      </span>
    </div>
  )
})

interface LogViewerProps {
  /** 日志行（按时间顺序） */
  lines: string[]
  /** 清空回调（仅清空前端显示）；不提供时隐藏清空按钮 */
  onClear?: () => void
  /** 滚动区最大高度（px），默认 400 */
  maxHeight?: number
  /** 导出文件名；缺省时生成带时间戳的 logs-*.txt */
  exportName?: string
  className?: string
}

/**
 * 终端风格日志查看器。
 *
 * - 等宽字体 + 行号 + 深色背景；
 * - 新日志到达自动滚动到底部（锁定状态），用户向上翻阅时自动解锁，
 *   点击「回到底部」重新锁定；
 * - 搜索（大小写不敏感、命中高亮）+ 级别过滤（ERROR/WARN/INFO，按级别着色）；
 * - 导出：将当前完整缓冲下载为 .txt；
 * - 「清空」仅清除前端显示，不影响服务端日志缓冲。
 */
export function LogViewer({
  lines,
  onClear,
  maxHeight = 400,
  exportName,
  className,
}: LogViewerProps) {
  const { t } = useTranslation('components')
  const scrollRef = useRef<HTMLDivElement>(null)
  const [locked, setLocked] = useState(true)
  const [query, setQuery] = useState('')
  const [level, setLevel] = useState<LogLevelFilter>('all')

  const trimmedQuery = query.trim().toLowerCase()

  // 过滤 + 级别识别（搜索/级别变化或缓冲更新时才重算）
  const rows = useMemo<LogRowData[]>(() => {
    const out: LogRowData[] = []
    lines.forEach((line, i) => {
      const lvl = detectLevel(line)
      if (level !== 'all' && lvl !== level) return
      if (trimmedQuery && !line.toLowerCase().includes(trimmedQuery)) return
      out.push({ line, num: i + 1, level: lvl })
    })
    return out
  }, [lines, level, trimmedQuery])

  // 锁定状态下，日志/过滤结果变化后始终贴底
  useLayoutEffect(() => {
    const el = scrollRef.current
    if (el && locked) el.scrollTop = el.scrollHeight
  }, [rows, locked])

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

  /** 将当前完整缓冲导出为 .txt（不受搜索/级别过滤影响） */
  const handleExport = () => {
    const name =
      exportName ??
      `logs-${new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-')}.txt`
    const blob = new Blob([lines.join('\n')], {
      type: 'text/plain;charset=utf-8',
    })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = name
    anchor.click()
    URL.revokeObjectURL(url)
  }

  return (
    <div className={cn('space-y-2', className)}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs text-muted-foreground">
          {t('logViewer.lineCount', { count: lines.length })}
          {trimmedQuery && (
            <span className="text-primary">
              {' '}
              · {t('logViewer.matchCount', { count: rows.length })}
            </span>
          )}
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-1.5">
          <div className="relative">
            <Search
              className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground/60"
              aria-hidden
            />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t('logViewer.searchPlaceholder')}
              aria-label={t('logViewer.searchLabel')}
              className="h-6 w-44 rounded-md pl-7 text-xs"
            />
          </div>
          <Select
            value={level}
            onValueChange={(v) => setLevel(v as LogLevelFilter)}
          >
            <SelectTrigger
              size="sm"
              aria-label={t('logViewer.levelFilterLabel')}
              className="h-6 gap-1 rounded-md px-2 text-xs data-[size=sm]:h-6"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t('logViewer.allLevels')}</SelectItem>
              <SelectItem value="error">ERROR</SelectItem>
              <SelectItem value="warn">WARN</SelectItem>
              <SelectItem value="info">INFO</SelectItem>
            </SelectContent>
          </Select>
          {locked ? (
            <Button
              variant="secondary"
              size="xs"
              onClick={followBottom}
              title={t('logViewer.lockedTitle')}
            >
              <Lock />
              {t('logViewer.following')}
            </Button>
          ) : (
            <Button
              variant="outline"
              size="xs"
              onClick={followBottom}
              className="border-primary/50 text-primary"
              title={t('logViewer.newLogsTitle')}
            >
              <ArrowDownToLine />
              {t('logViewer.backToBottom')}
            </Button>
          )}
          <Button
            variant="ghost"
            size="xs"
            onClick={handleExport}
            disabled={lines.length === 0}
            title={t('logViewer.exportTitle')}
          >
            <Download />
            {t('common:action.export')}
          </Button>
          {onClear && (
            <Button
              variant="ghost"
              size="xs"
              onClick={onClear}
              disabled={lines.length === 0}
              title={t('logViewer.clearTitle')}
            >
              <Trash2 />
              {t('logViewer.clear')}
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
        {rows.length === 0 ? (
          <div className="flex h-24 items-center justify-center text-muted-foreground/60">
            {lines.length === 0
              ? t('logViewer.noLogs')
              : t('logViewer.noMatches')}
          </div>
        ) : (
          <div className="py-2">
            {rows.map((row) => (
              <LogRow key={row.num} row={row} query={trimmedQuery} />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
