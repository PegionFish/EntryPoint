import { Component, type ErrorInfo, type ReactNode } from 'react'
import { TriangleAlert } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

/** fallback 中错误消息最多展示的字符数，防止超长堆栈/JSON 撑爆卡片 */
const ERROR_MESSAGE_MAX_LENGTH = 300

/** 把任意抛出值归一化为 Error（class 边界可能收到字符串或对象，而非 Error 实例） */
function normalizeError(thrown: unknown): Error {
  if (thrown instanceof Error) return thrown
  if (typeof thrown === 'string') return new Error(thrown)
  try {
    return new Error(JSON.stringify(thrown))
  } catch {
    return new Error(String(thrown))
  }
}

/** 截断错误消息（去除首尾空白），超长时以省略号收尾 */
function truncateErrorMessage(message: string): string {
  const text = message.trim()
  return text.length > ERROR_MESSAGE_MAX_LENGTH
    ? `${text.slice(0, ERROR_MESSAGE_MAX_LENGTH)}…`
    : text
}

/** 内置降级 UI：设计系统 Card + 危险色图标 + 错误摘要 + 重载按钮 */
function DefaultErrorFallback({ error }: { error: Error }) {
  const { t } = useTranslation('components')
  const message = truncateErrorMessage(error.message) || t('errorBoundary.unknownError')

  return (
    <div
      role="alert"
      className="flex h-full items-center justify-center overflow-auto p-6"
    >
      <Card className="w-full max-w-xl gap-4 py-6">
        <CardHeader className="gap-2">
          <div className="flex size-10 items-center justify-center rounded-lg bg-destructive/10 text-destructive">
            <TriangleAlert className="size-5" />
          </div>
          <CardTitle className="text-base">{t('errorBoundary.title')}</CardTitle>
          <CardDescription>{t('errorBoundary.description')}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* 错误摘要：等宽小字号、最多 300 字符、超长区域内部滚动 */}
          <p className="max-h-32 overflow-auto rounded-md border border-border bg-muted/40 px-3 py-2 font-mono text-xs break-all whitespace-pre-wrap text-muted-foreground">
            {message}
          </p>
          <Button size="sm" onClick={() => window.location.reload()}>
            {t('errorBoundary.reload')}
          </Button>
        </CardContent>
      </Card>
    </div>
  )
}

export interface ErrorBoundaryProps {
  children: ReactNode
  /** 自定义降级 UI：ReactNode，或接收错误对象的渲染函数；缺省使用内置错误卡片 */
  fallback?: ReactNode | ((error: Error) => ReactNode)
}

interface ErrorBoundaryState {
  error: Error | null
}

/**
 * 全局错误边界：子树渲染抛错时降级为可恢复错误卡片，而不是整页白屏。
 * 约定只包裹路由区域（见 App.tsx），Header/Sidebar 保持存活以便导航离开；
 * 路由切换时由调用方以 key 重建边界，从而清除错误态、恢复正常渲染。
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null }

  static getDerivedStateFromError(error: unknown): ErrorBoundaryState {
    return { error: normalizeError(error) }
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // 保留组件栈便于定位崩溃来源；记录本身不影响降级 UI 展示
    console.error('[ErrorBoundary]', error, info.componentStack)
  }

  render(): ReactNode {
    const { error } = this.state
    const { fallback, children } = this.props
    if (!error) return children
    if (typeof fallback === 'function') return fallback(error)
    return fallback === undefined ? <DefaultErrorFallback error={error} /> : fallback
  }
}
