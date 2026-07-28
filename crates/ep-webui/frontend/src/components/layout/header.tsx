import { LoaderCircle, Moon, Sun, Wifi, WifiOff } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useThemeStore } from '@/store/theme'
import { useWsState } from '@/hooks/use-ws-state'
import type { WsConnectionState } from '@/api/ws'
import { cn } from '@/lib/utils'

const WS_LABEL: Record<WsConnectionState, string> = {
  idle: '未连接',
  connecting: '连接中',
  connected: '已连接',
  reconnecting: '重连中',
  disconnected: '已断开',
}

function WsIndicator() {
  const state = useWsState()
  const connected = state === 'connected'
  const pending = state === 'connecting' || state === 'reconnecting'
  return (
    <div
      className="flex items-center gap-2 text-xs text-muted-foreground"
      title={`WebSocket ${WS_LABEL[state]}`}
    >
      <span
        className={cn(
          'h-2 w-2 rounded-full',
          connected
            ? 'bg-status-running'
            : pending
              ? 'animate-pulse bg-status-preparing'
              : 'bg-status-error',
        )}
      />
      {pending ? (
        <LoaderCircle className="h-3.5 w-3.5 animate-spin" />
      ) : connected ? (
        <Wifi className="h-3.5 w-3.5" />
      ) : (
        <WifiOff className="h-3.5 w-3.5" />
      )}
      <span className="hidden sm:inline">{WS_LABEL[state]}</span>
    </div>
  )
}

export function Header() {
  const theme = useThemeStore((s) => s.theme)
  const toggle = useThemeStore((s) => s.toggle)
  return (
    <header className="flex h-14 shrink-0 items-center justify-between border-b border-border bg-card px-4">
      <div className="flex items-center gap-2">
        <span className="text-base font-semibold tracking-tight">
          EntryPoint
        </span>
      </div>
      <div className="flex items-center gap-3">
        <WsIndicator />
        <Button
          variant="ghost"
          size="icon"
          onClick={toggle}
          aria-label="切换主题"
          title={theme === 'dark' ? '切换到浅色主题' : '切换到深色主题'}
        >
          {theme === 'dark' ? (
            <Sun className="h-4 w-4" />
          ) : (
            <Moon className="h-4 w-4" />
          )}
        </Button>
      </div>
    </header>
  )
}
