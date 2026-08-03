import { Construction } from 'lucide-react'
import { useTranslation } from 'react-i18next'

interface PlaceholderProps {
  message?: string
}

/** 占位内容：用于尚未实现的页面 */
export function Placeholder({ message }: PlaceholderProps) {
  const { t } = useTranslation('components')
  return (
    <div className="flex min-h-[40vh] flex-col items-center justify-center gap-3 text-muted-foreground">
      <Construction className="h-10 w-10 opacity-60" />
      <p className="text-sm">{message ?? t('placeholder.underConstruction')}</p>
    </div>
  )
}
