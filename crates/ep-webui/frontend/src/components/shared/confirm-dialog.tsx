import { useState } from 'react'
import { createRoot } from 'react-dom/client'
import { Loader2, TriangleAlert } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

export type ConfirmDialogVariant = 'default' | 'destructive'

export interface ConfirmDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string
  confirmLabel?: string
  cancelLabel?: string
  /** destructive：红色确认按钮 + 警告图标，用于停止 / 删除等操作 */
  variant?: ConfirmDialogVariant
  /** 支持返回 Promise：期间确认按钮显示加载态，失败时保持打开以便重试 */
  onConfirm: () => void | Promise<void>
}

/**
 * 确认对话框（受控）。
 *
 * 危险操作（variant="destructive"）使用红色确认按钮并默认聚焦"取消"，
 * 避免用户误触；异步 onConfirm 期间禁止通过 ESC / 遮罩关闭。
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel = '确认',
  cancelLabel = '取消',
  variant = 'default',
  onConfirm,
}: ConfirmDialogProps) {
  const [pending, setPending] = useState(false)
  const destructive = variant === 'destructive'

  // 异步操作进行中时拦截关闭（ESC / 点击遮罩 / 右上角关闭）
  const handleOpenChange = (next: boolean) => {
    if (pending && !next) return
    onOpenChange(next)
  }

  const handleConfirm = async () => {
    const result = onConfirm()
    if (result instanceof Promise) {
      setPending(true)
      try {
        await result
      } catch {
        // 失败时保持打开，让用户可以重试
        setPending(false)
        return
      }
      setPending(false)
    }
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <div className="flex items-start gap-3">
            {destructive && (
              <div className="flex size-10 shrink-0 items-center justify-center rounded-lg border border-destructive/20 bg-destructive/10 text-destructive">
                <TriangleAlert className="size-5" />
              </div>
            )}
            <div className="space-y-1.5">
              <DialogTitle>{title}</DialogTitle>
              {description ? (
                <DialogDescription>{description}</DialogDescription>
              ) : (
                // Radix 要求 Dialog 提供可访问性描述
                <DialogDescription className="sr-only">{title}</DialogDescription>
              )}
            </div>
          </div>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={pending}
            autoFocus={destructive}
          >
            {cancelLabel}
          </Button>
          <Button
            variant={destructive ? 'destructive' : 'default'}
            onClick={handleConfirm}
            disabled={pending}
          >
            {pending && <Loader2 className="animate-spin" />}
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export interface ConfirmDialogOptions {
  title: string
  description?: string
  confirmLabel?: string
  cancelLabel?: string
  variant?: ConfirmDialogVariant
}

/**
 * 命令式确认对话框：`const ok = await confirmDialog({ title, description, variant })`。
 *
 * 无需在组件中维护 open 状态；返回 Promise<boolean>，
 * 用户确认返回 true，取消（按钮 / ESC / 遮罩 / 关闭图标）返回 false。
 */
export function confirmDialog(options: ConfirmDialogOptions): Promise<boolean> {
  return new Promise((resolve) => {
    const container = document.createElement('div')
    document.body.appendChild(container)
    const root = createRoot(container)
    let settled = false

    const settle = (value: boolean) => {
      if (settled) return
      settled = true
      resolve(value)
      // 先以 open=false 重渲染播放关闭动画，再卸载
      root.render(renderDialog(false))
      window.setTimeout(() => {
        root.unmount()
        container.remove()
      }, 200)
    }

    const renderDialog = (dialogOpen: boolean) => (
      <ConfirmDialog
        open={dialogOpen}
        onOpenChange={(next) => {
          if (!next) settle(false)
        }}
        title={options.title}
        description={options.description}
        confirmLabel={options.confirmLabel}
        cancelLabel={options.cancelLabel}
        variant={options.variant}
        onConfirm={() => settle(true)}
      />
    )

    root.render(renderDialog(true))
  })
}
