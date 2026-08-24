import { useRef } from 'react'
import type { ChangeEvent } from 'react'
import { CircleCheck, FolderOpen, GitBranch, PanelLeft, Play, Save } from 'lucide-react'
import { toast } from 'sonner'
import {
  Download,
  FileUp,
  Timer,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import type { PipelineDefinition } from '@/components/shared/pipeline-node'

interface PipelineToolbarProps {
  /** 管线名称（可编辑） */
  name: string
  onNameChange: (name: string) => void
  nodeCount: number
  edgeCount: number
  /** 窄屏（<lg）节点库抽屉是否展开 */
  libraryOpen: boolean
  onToggleLibrary: () => void
  onSave: () => void
  onLoad: (def: PipelineDefinition) => void
  onValidate: () => void
  onExecute: () => void
  /** 导出当前服务端管线为分享 JSON（无 currentId 时禁用） */
  canExport: boolean
  onExport: () => void
  /** 导入分享 JSON → 直接创建服务端管线 */
  onImportShare: (file: File) => void
  /** 打开定时调度对话框（需 currentId） */
  canSchedule: boolean
  onSchedule: () => void
}

/** 管线编辑器工具栏：节点库开关 + 命名 + 统计 + 保存 / 加载 / 验证 / 执行 */
export function PipelineToolbar({
  name,
  onNameChange,
  nodeCount,
  edgeCount,
  libraryOpen,
  onToggleLibrary,
  onSave,
  onLoad,
  onValidate,
  onExecute,
  canExport,
  onExport,
  onImportShare,
  canSchedule,
  onSchedule,
}: PipelineToolbarProps) {
  const { t } = useTranslation('components')
  const fileRef = useRef<HTMLInputElement>(null)
  const shareRef = useRef<HTMLInputElement>(null)

  const handleShareFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    event.target.value = ''
    if (!file) return
    onImportShare(file)
  }

  const handleFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    event.target.value = ''
    if (!file) return
    try {
      const parsed = JSON.parse(await file.text()) as Partial<PipelineDefinition>
      if (!parsed || !Array.isArray(parsed.nodes)) {
        // 内部控制流消息（不直接展示，统一由下方 toast 提示）
        throw new Error('missing "nodes" array')
      }
      onLoad({
        name: typeof parsed.name === 'string' ? parsed.name : file.name.replace(/\.json$/i, ''),
        version: typeof parsed.version === 'number' ? parsed.version : 1,
        nodes: parsed.nodes,
        edges: Array.isArray(parsed.edges) ? parsed.edges : [],
      })
    } catch {
      toast.error(t('pipelineToolbar.parseError'), {
        description: t('pipelineToolbar.parseErrorDescription'),
      })
    }
  }

  return (
    <div className="glass flex h-14 shrink-0 items-center gap-2 border-b border-border-glow px-3 md:gap-3 md:px-4">
      <Button
        variant="ghost"
        size="icon-sm"
        className="shrink-0 lg:hidden"
        onClick={onToggleLibrary}
        aria-label={
          libraryOpen
            ? t('pipelineSidebar.closeLibrary')
            : t('pipelineToolbar.openLibrary')
        }
        aria-pressed={libraryOpen}
        title={t('pipelineSidebar.title')}
      >
        <PanelLeft className="h-4 w-4" />
      </Button>
      <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
        <GitBranch className="h-4 w-4" />
      </span>
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <h1 className="hidden shrink-0 text-base font-semibold tracking-tight sm:block">
          {t('pipelineToolbar.pageTitle')}
        </h1>
        <input
          value={name}
          onChange={(e) => onNameChange(e.target.value)}
          placeholder={t('pipelineToolbar.unnamedPipeline')}
          aria-label={t('pipelineToolbar.nameLabel')}
          className="h-8 w-full min-w-0 max-w-xs flex-1 truncate rounded-md border border-transparent bg-transparent px-2 text-sm outline-none transition-colors placeholder:text-muted-foreground hover:border-input focus-visible:border-ring focus-visible:bg-background focus-visible:shadow-[0_0_0_3px_var(--ring-glow)]"
        />
      </div>
      <span className="hidden shrink-0 font-mono text-xs text-muted-foreground lg:inline">
        {t('pipelineToolbar.stats', { nodes: nodeCount, edges: edgeCount })}
      </span>

      <div className="flex shrink-0 items-center gap-1.5 md:gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={onSave}
          title={t('pipelineToolbar.saveTitle')}
          aria-label={t('pipelineToolbar.save')}
        >
          <Save className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.save')}</span>
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => fileRef.current?.click()}
          title={t('pipelineToolbar.loadTitle')}
          aria-label={t('pipelineToolbar.load')}
        >
          <FolderOpen className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.load')}</span>
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!canExport}
          onClick={onExport}
          title={t('pipelineToolbar.exportTitle')}
          aria-label={t('pipelineToolbar.export')}
        >
          <Download className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.export')}</span>
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => shareRef.current?.click()}
          title={t('pipelineToolbar.importShareTitle')}
          aria-label={t('pipelineToolbar.importShare')}
        >
          <FileUp className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.importShare')}</span>
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={!canSchedule}
          onClick={onSchedule}
          title={t('pipelineToolbar.scheduleTitle')}
          aria-label={t('pipelineToolbar.schedule')}
        >
          <Timer className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.schedule')}</span>
        </Button>
        <Separator orientation="vertical" className="mx-0.5 h-5" />
        <Button
          variant="outline"
          size="sm"
          onClick={onValidate}
          title={t('pipelineToolbar.validateTitle')}
          aria-label={t('pipelineToolbar.validateAria')}
        >
          <CircleCheck className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('pipelineToolbar.validate')}</span>
        </Button>
        <Button
          size="sm"
          onClick={onExecute}
          title={t('pipelineToolbar.executeTitle')}
          aria-label={t('pipelineToolbar.executeAria')}
        >
          <Play className="h-3.5 w-3.5" />
          <span className="hidden md:inline">{t('common:action.execute')}</span>
        </Button>
      </div>

      <input
        ref={fileRef}
        type="file"
        accept=".json,application/json"
        className="hidden"
        onChange={handleFile}
        aria-hidden="true"
        tabIndex={-1}
      />
        <input
          ref={shareRef}
          type="file"
          accept="application/json,.json"
          className="hidden"
          onChange={handleShareFile}
        />
    </div>
  )
}
