import { LoaderCircle, Menu, Moon, Sun, Wifi, WifiOff } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { useThemeStore } from '@/store/theme'
import { useWsState } from '@/hooks/use-ws-state'
import { cn } from '@/lib/utils'

function WsIndicator() {
  const { t } = useTranslation('components')
  const state = useWsState()
  const label = t(`header.ws.${state}`)
  const connected = state === 'connected'
  const pending = state === 'connecting' || state === 'reconnecting'
  return (
    <div
      className="flex items-center gap-2 text-xs text-muted-foreground"
      title={t('header.wsTitle', { state: label })}
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
      <span className="hidden sm:inline">{label}</span>
    </div>
  )
}

interface HeaderProps {
  /** 点击汉堡按钮时打开移动端导航抽屉 */
  onMenuClick: () => void
}

export function Header({ onMenuClick }: HeaderProps) {
  const { t } = useTranslation('components')
  const theme = useThemeStore((s) => s.theme)
  const toggle = useThemeStore((s) => s.toggle)
  return (
    <header className="flex h-14 shrink-0 items-center justify-between border-b border-border bg-card px-4">
      <div className="flex items-center gap-2">
        <Button
          variant="ghost"
          size="icon"
          className="lg:hidden"
          onClick={onMenuClick}
          aria-label={t('header.openNav')}
          title={t('header.openNav')}
        >
          <Menu className="h-4 w-4" />
        </Button>
        {/* 品牌字标：电光青→靛渐变（W4 渐变补齐，§3.1 许可位） */}
        <span className="text-gradient text-base font-semibold tracking-tight">
          EntryPoint
        </span>
      </div>
      <div className="flex items-center gap-3">
        <WsIndicator />
        <Button
          variant="ghost"
          size="icon"
          onClick={toggle}
          aria-label={t('header.toggleTheme')}
          title={theme === 'dark' ? t('header.switchToLight') : t('header.switchToDark')}
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
