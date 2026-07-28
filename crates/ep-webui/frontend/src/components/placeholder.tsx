import { Construction } from 'lucide-react'

interface PlaceholderProps {
  message?: string
}

/** 占位内容：用于尚未实现的页面 */
export function Placeholder({ message = '页面开发中' }: PlaceholderProps) {
  return (
    <div className="flex min-h-[40vh] flex-col items-center justify-center gap-3 text-muted-foreground">
      <Construction className="h-10 w-10 opacity-60" />
      <p className="text-sm">{message}</p>
    </div>
  )
}
