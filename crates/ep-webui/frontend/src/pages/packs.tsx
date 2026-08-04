import { useTranslation } from 'react-i18next'
import { PackageOpen } from 'lucide-react'
import { PageContainer } from '@/components/layout/page-container'

/**
 * 整合包管理页（Wave S S2 骨架注册点）。
 *
 * 当前仅页面骨架：标题 + 空态占位。
 * 完整实现（已装包列表 / 导入（本地/URL/上传）/ 构建导出 / 卸载 / 适配报告）
 * 按 PACK_UNIFY_PLAN §4 与 §8.1 契约，由后续波次代理（C1）在本文件内填充——
 * API 方法已在 api/client.ts 预注册（listPacks/importPack/uploadPack/buildPack/
 * packExportUrl/deletePack/getPack），契约类型见 api/types.ts 的 Packs 段。
 */
export function PacksPage() {
  const { t } = useTranslation('packs')
  return (
    <PageContainer
      title={t('page.title', { defaultValue: '整合包' })}
      description={t('page.description', {
        defaultValue: '导入、构建与管理模型整合包（.epzip）',
      })}
    >
      {/* 整合包管理区骨架：空态占位（C1 后续实现列表/导入/构建/卸载） */}
      <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border py-16 text-center">
        <PackageOpen className="size-8 text-muted-foreground/50" />
        <p className="text-sm text-muted-foreground">
          {t('empty.title', { defaultValue: '暂无整合包' })}
        </p>
        <p className="text-xs text-muted-foreground/70">
          {t('empty.description', {
            defaultValue: '导入或构建整合包后将在此显示（页面实现待后续波次接入）',
          })}
        </p>
      </div>
    </PageContainer>
  )
}
