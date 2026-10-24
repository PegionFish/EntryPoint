import { useEffect, useState } from 'react'
import { Navigate, Route, Routes, useLocation } from 'react-router-dom'
import '@/i18n'
import { normalizeLanguage, setAppLanguage } from '@/i18n'
import { Header } from '@/components/layout/header'
import { MobileNav } from '@/components/layout/mobile-nav'
import { Sidebar } from '@/components/layout/sidebar'
import { ErrorBoundary } from '@/components/shared/error-boundary'
import { TooltipProvider } from '@/components/ui/tooltip'
import { Toaster } from '@/components/ui/sonner'
import { api } from '@/api/client'
import { wsManager } from '@/api/ws'
import { useModelUpdateToast } from '@/hooks/use-model-update-toast'
import { DashboardPage } from '@/pages/dashboard'
import { ModulesPage } from '@/pages/modules'
import { PipelinePage } from '@/pages/pipeline'
import { RunPage } from '@/pages/run'
import { TasksPage } from '@/pages/tasks'
import { TriggersPage } from '@/pages/triggers'
import { SettingsPage } from '@/pages/settings'
import { NotFoundPage } from '@/pages/not-found'

export default function App() {
  // 移动端（<lg）导航抽屉开关状态
  const [mobileNavOpen, setMobileNavOpen] = useState(false)
  // 当前路径用于重建错误边界：换页后自动清除上一页的崩溃态
  const location = useLocation()

  // 应用启动时建立全局 WebSocket 连接
  useEffect(() => {
    wsManager.connect()
    return () => wsManager.disconnect()
  }, [])

  // 全局常驻订阅 WS model_update（后台自动更新检查发现可用更新 → toast 提示）
  useModelUpdateToast()

  // 挂载时以服务器 config.general.language 为全局真源校准界面语言
  // （i18n 初始化时已用 localStorage 缓存防首屏闪烁，这里纠正偏差并回写缓存）
  useEffect(() => {
    api
      .getConfig()
      .then((cfg) => setAppLanguage(normalizeLanguage(cfg.general.language)))
      .catch(() => {
        // 服务器暂不可达时保留本地缓存语言，下次挂载再校准
      })
  }, [])

  return (
    <TooltipProvider>
      <div className="flex h-screen flex-col overflow-hidden">
        <Header onMenuClick={() => setMobileNavOpen(true)} />
        <div className="flex flex-1 overflow-hidden">
          <Sidebar />
          <main className="flex-1 overflow-hidden">
            {/* 仅包裹路由区域：Header/Sidebar 位于边界之外保持存活，页面崩溃时仍可导航离开。
                key 随路径变化重建边界，离开崩溃页后错误态自动清除。 */}
            <ErrorBoundary key={location.pathname}>
              <Routes>
                <Route path="/" element={<DashboardPage />} />
                <Route path="/modules" element={<ModulesPage />} />
                {/* W2：模块详情独立页裁撤，旧路由重定向回模块页（详情统一为抽屉） */}
                <Route path="/modules/:id" element={<Navigate to="/modules" replace />} />
                <Route path="/pipeline" element={<PipelinePage />} />
                {/* 触发器：目录监控 + 自动执行（PLAN_TRIGGER_UNIFIED_LOG D2 独立顶级页） */}
                <Route path="/triggers" element={<TriggersPage />} />
                {/* 快速调用：与管线同级的单模块 ad hoc 直跑（QUICK_RUN_PLAN D1） */}
                <Route path="/run" element={<RunPage />} />
                <Route path="/tasks" element={<TasksPage />} />
                <Route path="/settings" element={<SettingsPage />} />
                <Route path="*" element={<NotFoundPage />} />
              </Routes>
            </ErrorBoundary>
          </main>
        </div>
      </div>
      <MobileNav open={mobileNavOpen} onOpenChange={setMobileNavOpen} />
      <Toaster richColors closeButton position="top-right" />
    </TooltipProvider>
  )
}
