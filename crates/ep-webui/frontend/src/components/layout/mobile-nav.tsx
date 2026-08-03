import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { SidebarNav } from '@/components/layout/sidebar'

interface MobileNavProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

/**
 * 移动端（<lg）抽屉式导航。
 *
 * 复用 `SidebarNav` 保证与桌面侧栏导航数据源一致；
 * 点击遮罩 / 按 ESC / 点击导航项后自动关闭。
 */
export function MobileNav({ open, onOpenChange }: MobileNavProps) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="left" className="w-64 gap-0 bg-card sm:max-w-64">
        <SheetHeader className="shrink-0 border-b border-border">
          <SheetTitle className="text-base font-semibold tracking-tight">
            EntryPoint
          </SheetTitle>
          <SheetDescription className="sr-only">
            应用主导航菜单
          </SheetDescription>
        </SheetHeader>
        <SidebarNav
          className="flex-1 overflow-y-auto"
          onNavigate={() => onOpenChange(false)}
        />
        <div className="shrink-0 border-t border-border p-3 text-xs text-muted-foreground">
          EntryPoint WebUI
        </div>
      </SheetContent>
    </Sheet>
  )
}
