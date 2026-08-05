import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { wsManager } from '@/api/ws'

/**
 * 已提示过的更新去重键（module_id:model_id:reason，模块级存活）。
 * reason 含远端更新时间，故同一模型出现真正的新更新时会生成新键重新提示；
 * 同轮/重复广播（键相同）只提示一次。
 */
const notifiedKeys = new Set<string>()

/**
 * 全局常驻订阅 WS `model_update` 消息：后台自动更新检查发现可用更新时
 * 以 toast 提示，action 可跳转模块页。
 *
 * 挂载于 App 布局层（非页面级），保证用户在任何页面都能收到提示。
 */
export function useModelUpdateToast(): void {
  const { t } = useTranslation('modules')
  const navigate = useNavigate()

  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'model_update') return
      const key = `${msg.module_id}:${msg.model_id}:${msg.reason}`
      if (notifiedKeys.has(key)) return
      notifiedKeys.add(key)
      toast.info(
        t('ws.modelUpdate', {
          defaultValue: '{{module}} / {{model}} has an available update',
          module: msg.module_id,
          model: msg.model_id,
        }),
        {
          description: msg.reason,
          action: {
            label: t('ws.goModules', { defaultValue: 'Go to Modules' }),
            onClick: () => navigate('/modules'),
          },
        },
      )
    })
  }, [navigate, t])
}
