import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { PageContainer } from '@/components/layout/page-container'
import { Button } from '@/components/ui/button'

export function NotFoundPage() {
  const { t } = useTranslation('tasks')
  return (
    <PageContainer
      title={t('notFound.title')}
      description={t('notFound.description')}
    >
      <div className="flex min-h-[40vh] flex-col items-center justify-center gap-4 text-muted-foreground">
        <p className="text-4xl font-bold text-foreground">404</p>
        <p className="text-sm">{t('notFound.check')}</p>
        <Button asChild variant="outline">
          <Link to="/">{t('notFound.backToDashboard')}</Link>
        </Button>
      </div>
    </PageContainer>
  )
}
