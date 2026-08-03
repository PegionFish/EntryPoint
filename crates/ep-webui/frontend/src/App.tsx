import { useEffect, useState } from 'react'
import { Route, Routes } from 'react-router-dom'
import { Header } from '@/components/layout/header'
import { MobileNav } from '@/components/layout/mobile-nav'
import { Sidebar } from '@/components/layout/sidebar'
import { TooltipProvider } from '@/components/ui/tooltip'
import { Toaster } from '@/components/ui/sonner'
import { wsManager } from '@/api/ws'
import { DashboardPage } from '@/pages/dashboard'
import { ModulesPage } from '@/pages/modules'
import ModuleDetailPage from '@/pages/module-detail'
import { PipelinePage } from '@/pages/pipeline'
import { TasksPage } from '@/pages/tasks'
import { ModelsPage } from '@/pages/models'
import { SettingsPage } from '@/pages/settings'
import { NotFoundPage } from '@/pages/not-found'

export default function App() {
  // 移动端（<lg）导航抽屉开关状态
  const [mobileNavOpen, setMobileNavOpen] = useState(false)

  // 应用启动时建立全局 WebSocket 连接
  useEffect(() => {
    wsManager.connect()
    return () => wsManager.disconnect()
  }, [])

  return (
    <TooltipProvider>
      <div className="flex h-screen flex-col overflow-hidden">
        <Header onMenuClick={() => setMobileNavOpen(true)} />
        <div className="flex flex-1 overflow-hidden">
          <Sidebar />
          <main className="flex-1 overflow-hidden">
            <Routes>
              <Route path="/" element={<DashboardPage />} />
              <Route path="/modules" element={<ModulesPage />} />
              <Route path="/modules/:id" element={<ModuleDetailPage />} />
              <Route path="/pipeline" element={<PipelinePage />} />
              <Route path="/tasks" element={<TasksPage />} />
              <Route path="/models" element={<ModelsPage />} />
              <Route path="/settings" element={<SettingsPage />} />
              <Route path="*" element={<NotFoundPage />} />
            </Routes>
          </main>
        </div>
      </div>
      <MobileNav open={mobileNavOpen} onOpenChange={setMobileNavOpen} />
      <Toaster richColors closeButton position="top-right" />
    </TooltipProvider>
  )
}
