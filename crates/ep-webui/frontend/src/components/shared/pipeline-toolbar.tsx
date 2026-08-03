import { useRef } from 'react'
import type { ChangeEvent } from 'react'
import { CircleCheck, FolderOpen, GitBranch, PanelLeft, Play, Save } from 'lucide-react'
import { toast } from 'sonner'
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
}: PipelineToolbarProps) {
  const fileRef = useRef<HTMLInputElement>(null)

  const handleFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    event.target.value = ''
    if (!file) return
    try {
      const parsed = JSON.parse(await file.text()) as Partial<PipelineDefinition>
      if (!parsed || !Array.isArray(parsed.nodes)) {
        throw new Error('缺少 nodes 数组')
      }
      onLoad({
        name: typeof parsed.name === 'string' ? parsed.name : file.name.replace(/\.json$/i, ''),
        version: typeof parsed.version === 'number' ? parsed.version : 1,
        nodes: parsed.nodes,
        edges: Array.isArray(parsed.edges) ? parsed.edges : [],
      })
    } catch {
      toast.error('管线文件解析失败', { description: '请选择有效的管线 JSON 文件' })
    }
  }

  return (
    <div className="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-card px-3 md:gap-3 md:px-4">
      <Button
        variant="ghost"
        size="icon-sm"
        className="shrink-0 lg:hidden"
        onClick={onToggleLibrary}
        aria-label={libraryOpen ? '关闭节点库' : '打开节点库'}
        aria-pressed={libraryOpen}
        title="节点库"
      >
        <PanelLeft className="h-4 w-4" />
      </Button>
      <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
        <GitBranch className="h-4 w-4" />
      </span>
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <h1 className="hidden shrink-0 text-base font-semibold tracking-tight sm:block">
          管线编排
        </h1>
        <input
          value={name}
          onChange={(e) => onNameChange(e.target.value)}
          placeholder="未命名管线"
          aria-label="管线名称"
          className="h-8 w-full min-w-0 max-w-xs flex-1 truncate rounded-md border border-transparent bg-transparent px-2 text-sm outline-none transition-colors placeholder:text-muted-foreground hover:border-input focus-visible:border-ring focus-visible:bg-background focus-visible:ring-[3px] focus-visible:ring-ring/50"
        />
      </div>
      <span className="hidden shrink-0 font-mono text-xs text-muted-foreground lg:inline">
        {nodeCount} 节点 · {edgeCount} 连接
      </span>

      <div className="flex shrink-0 items-center gap-1.5 md:gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={onSave}
          title="导出管线定义文件"
          aria-label="保存管线"
        >
          <Save className="h-3.5 w-3.5" />
          <span className="hidden md:inline">保存管线</span>
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => fileRef.current?.click()}
          title="从文件加载管线定义"
          aria-label="加载管线"
        >
          <FolderOpen className="h-3.5 w-3.5" />
          <span className="hidden md:inline">加载管线</span>
        </Button>
        <Separator orientation="vertical" className="mx-0.5 h-5" />
        <Button
          variant="outline"
          size="sm"
          onClick={onValidate}
          title="检查节点连接完整性"
          aria-label="验证管线"
        >
          <CircleCheck className="h-3.5 w-3.5" />
          <span className="hidden md:inline">验证</span>
        </Button>
        <Button size="sm" onClick={onExecute} title="提交管线执行" aria-label="执行管线">
          <Play className="h-3.5 w-3.5" />
          <span className="hidden md:inline">执行</span>
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
    </div>
  )
}
