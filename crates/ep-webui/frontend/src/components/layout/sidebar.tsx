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

export function Sidebar() {
  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-border bg-card">
      <nav className="flex flex-1 flex-col gap-1 overflow-y-auto p-3">
        {NAV_ITEMS.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.end}
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
      <div className="border-t border-border p-3 text-xs text-muted-foreground">
        EntryPoint WebUI
      </div>
    </aside>
  )
}
