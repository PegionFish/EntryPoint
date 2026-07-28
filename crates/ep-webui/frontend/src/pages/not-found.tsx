import { Link } from 'react-router-dom'
import { PageContainer } from '@/components/layout/page-container'
import { Button } from '@/components/ui/button'

export function NotFoundPage() {
  return (
    <PageContainer title="页面未找到" description="您访问的页面不存在">
      <div className="flex min-h-[40vh] flex-col items-center justify-center gap-4 text-muted-foreground">
        <p className="text-4xl font-bold text-foreground">404</p>
        <p className="text-sm">请检查地址是否正确</p>
        <Button asChild variant="outline">
          <Link to="/">返回仪表盘</Link>
        </Button>
      </div>
    </PageContainer>
  )
}
