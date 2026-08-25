import { NavLink } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import {
  GitBranch,
  LayoutDashboard,
  ListTodo,
  Puzzle,
  Settings,
  Zap,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'

interface NavItem {
  to: string
  /** components 命名空间下的标签键 */
  labelKey: string
  icon: LucideIcon
  end?: boolean
}

const NAV_ITEMS: NavItem[] = [
  { to: '/', labelKey: 'sidebar.nav.dashboard', icon: LayoutDashboard, end: true },
  { to: '/modules', labelKey: 'sidebar.nav.modules', icon: Puzzle },
  { to: '/pipeline', labelKey: 'sidebar.nav.pipeline', icon: GitBranch },
  { to: '/run', labelKey: 'sidebar.nav.quickrun', icon: Zap },
  { to: '/tasks', labelKey: 'sidebar.nav.tasks', icon: ListTodo },
  { to: '/settings', labelKey: 'sidebar.nav.settings', icon: Settings },
]

interface SidebarNavProps {
  /** 点击导航项后的回调（用于关闭移动端抽屉等场景） */
  onNavigate?: () => void
  className?: string
}

/** 导航列表：桌面侧栏与移动端抽屉共用，保证导航数据源与样式单一 */
export function SidebarNav({ onNavigate, className }: SidebarNavProps) {
  const { t } = useTranslation('components')
  return (
    <nav className={cn('flex flex-col gap-1 p-3', className)}>
      {NAV_ITEMS.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.end}
          onClick={onNavigate}
          className={({ isActive }) =>
            cn(
              'relative flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors',
              isActive
                ? 'bg-primary/10 text-primary'
                : 'text-muted-foreground hover:bg-accent hover:text-foreground',
            )
          }
        >
          {({ isActive }) => (
            <>
              {/* 激活指示条：2px 青→靛纵向渐变竖条，与桌面端导航语义同步（W4 渐变补齐） */}
              {isActive && (
                <span
                  aria-hidden
                  className="bg-gradient-accent-vertical absolute top-1/2 left-0 h-5 w-0.5 -translate-y-1/2 rounded-full"
                />
              )}
              <item.icon className="h-4 w-4 shrink-0" />
              <span>{t(item.labelKey)}</span>
            </>
          )}
        </NavLink>
      ))}
    </nav>
  )
}

export function Sidebar() {
  return (
    <aside className="hidden w-56 shrink-0 flex-col border-r border-border bg-card lg:flex">
      <SidebarNav className="flex-1 overflow-y-auto" />
      <div className="border-t border-border p-3 text-xs text-muted-foreground">
        EntryPoint WebUI
      </div>
    </aside>
  )
}
