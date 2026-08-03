import { NavLink } from 'react-router-dom'
import {
  Database,
  GitBranch,
  LayoutDashboard,
  ListTodo,
  Puzzle,
  Settings,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'

interface NavItem {
  to: string
  label: string
  icon: LucideIcon
  end?: boolean
}

const NAV_ITEMS: NavItem[] = [
  { to: '/', label: '仪表盘', icon: LayoutDashboard, end: true },
  { to: '/modules', label: '模块', icon: Puzzle },
  { to: '/pipeline', label: '管线', icon: GitBranch },
  { to: '/tasks', label: '任务', icon: ListTodo },
  { to: '/models', label: '模型', icon: Database },
  { to: '/settings', label: '设置', icon: Settings },
]

interface SidebarNavProps {
  /** 点击导航项后的回调（用于关闭移动端抽屉等场景） */
  onNavigate?: () => void
  className?: string
}

/** 导航列表：桌面侧栏与移动端抽屉共用，保证导航数据源与样式单一 */
export function SidebarNav({ onNavigate, className }: SidebarNavProps) {
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
              'flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors',
              isActive
                ? 'bg-primary/10 text-primary'
                : 'text-muted-foreground hover:bg-accent hover:text-foreground',
            )
          }
        >
          <item.icon className="h-4 w-4 shrink-0" />
          <span>{item.label}</span>
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
