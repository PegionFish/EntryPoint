import { useCallback, useState } from 'react'
import {
  ReactFlow,
  Background,
  Controls,
  MiniMap,
  addEdge,
  type Node,
  type Edge,
  type Connection,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { toast } from 'sonner'
import { CATEGORY_LABELS } from '@/lib/constants'
import { useModules } from '@/hooks/use-modules'
import {
  Save,
  FolderOpen,
  CheckCircle,
  Play,
  Puzzle,
  FileInput,
  FileOutput,
  Film,
} from 'lucide-react'

const BUILTIN_NODES = [
  { type: 'file_input', label: '文件输入', icon: FileInput },
  { type: 'file_output', label: '文件输出', icon: FileOutput },
  { type: 'ffmpeg', label: 'FFmpeg', icon: Film },
]

let nodeId = 0
const getId = () => `node_${nodeId++}`

export function PipelinePage() {
  const [nodes, setNodes] = useState<Node[]>([])
  const [edges, setEdges] = useState<Edge[]>([])
  const { modules } = useModules()

  const onConnect = useCallback(
    (params: Connection) => setEdges((eds) => addEdge(params, eds)),
    [],
  )

  const addNode = useCallback(
    (label: string, type: string) => {
      const id = getId()
      setNodes((nds) => [
        ...nds,
        {
          id,
          type: 'default',
          position: { x: 100 + Math.random() * 300, y: 100 + Math.random() * 200 },
          data: { label: `${label}\n(${type})` },
        },
      ])
    },
    [],
  )

  const handleSave = () => {
    const pipeline = { nodes, edges }
    const blob = new Blob([JSON.stringify(pipeline, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = 'pipeline.json'
    a.click()
    URL.revokeObjectURL(url)
    toast.success('管线已保存')
  }

  const handleLoad = () => {
    const input = document.createElement('input')
    input.type = 'file'
    input.accept = '.json'
    input.onchange = (e) => {
      const file = (e.target as HTMLInputElement).files?.[0]
      if (!file) return
      const reader = new FileReader()
      reader.onload = (ev) => {
        try {
          const data = JSON.parse(ev.target?.result as string)
          if (data.nodes) setNodes(data.nodes)
          if (data.edges) setEdges(data.edges)
          toast.success('管线已加载')
        } catch {
          toast.error('加载失败：无效的管线文件')
        }
      }
      reader.readAsText(file)
    }
    input.click()
  }

  const handleValidate = () => {
    if (nodes.length === 0) {
      toast.warning('管线为空，请先添加节点')
      return
    }
    toast.success(`管线验证通过：${nodes.length} 个节点，${edges.length} 条连线`)
  }

  const handleExecute = () => {
    toast.info('管线执行功能开发中')
  }

  const grouped = (modules ?? []).reduce<Record<string, typeof modules>>((acc, m) => {
    const cat = m.category || 'custom'
    if (!acc[cat]) acc[cat] = []
    acc[cat].push(m)
    return acc
  }, {})

  return (
    <div className="flex h-[calc(100vh-3.5rem)] flex-col">
      {/* 工具栏 */}
      <div className="flex items-center gap-2 border-b px-4 py-2">
        <h1 className="mr-4 text-lg font-semibold">管线编辑器</h1>
        <Button variant="outline" size="sm" onClick={handleSave}>
          <Save className="mr-1 h-4 w-4" /> 保存
        </Button>
        <Button variant="outline" size="sm" onClick={handleLoad}>
          <FolderOpen className="mr-1 h-4 w-4" /> 加载
        </Button>
        <Button variant="outline" size="sm" onClick={handleValidate}>
          <CheckCircle className="mr-1 h-4 w-4" /> 验证
        </Button>
        <Button size="sm" onClick={handleExecute}>
          <Play className="mr-1 h-4 w-4" /> 执行
        </Button>
        <div className="ml-auto text-sm text-muted-foreground">
          {nodes.length} 节点 · {edges.length} 连线
        </div>
      </div>

      <div className="flex flex-1 overflow-hidden">
        {/* 左侧节点面板 */}
        <div className="w-56 overflow-y-auto border-r p-3">
          <p className="mb-2 text-xs font-medium text-muted-foreground">内置节点</p>
          <div className="mb-4 space-y-1">
            {BUILTIN_NODES.map((n) => (
              <button
                key={n.type}
                className="flex w-full items-center gap-2 rounded-md border px-3 py-2 text-sm hover:bg-accent"
                onClick={() => addNode(n.label, n.type)}
              >
                <n.icon className="h-4 w-4 text-muted-foreground" />
                {n.label}
              </button>
            ))}
          </div>

          <p className="mb-2 text-xs font-medium text-muted-foreground">模块节点</p>
          <div className="space-y-3">
            {Object.entries(grouped).map(([cat, mods]) => (
              <div key={cat}>
                <p className="mb-1 text-xs text-muted-foreground">
                  {CATEGORY_LABELS[cat] ?? cat}
                </p>
                {mods.map((m) => (
                  <button
                    key={m.id}
                    className="mb-1 flex w-full items-center gap-2 rounded-md border px-3 py-2 text-sm hover:bg-accent"
                    onClick={() => addNode(m.name, m.id)}
                  >
                    <Puzzle className="h-4 w-4 text-muted-foreground" />
                    <span className="truncate">{m.name}</span>
                    <Badge variant="secondary" className="ml-auto text-[10px]">
                      v{m.version}
                    </Badge>
                  </button>
                ))}
              </div>
            ))}
          </div>
        </div>

        {/* React Flow 画布 */}
        <div className="flex-1">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onConnect={onConnect}
            onNodesChange={(changes) => {
              setNodes((nds) => {
                const updated = [...nds]
                for (const c of changes) {
                  if (c.type === 'position' && c.position) {
                    const idx = updated.findIndex((n) => n.id === c.id)
                    if (idx >= 0) updated[idx] = { ...updated[idx], position: c.position }
                  }
                  if (c.type === 'remove') {
                    const idx = updated.findIndex((n) => n.id === c.id)
                    if (idx >= 0) updated.splice(idx, 1)
                  }
                }
                return updated
              })
            }}
            onEdgesChange={(changes) => {
              setEdges((eds) => {
                const updated = [...eds]
                for (const c of changes) {
                  if (c.type === 'remove') {
                    const idx = updated.findIndex((e) => e.id === c.id)
                    if (idx >= 0) updated.splice(idx, 1)
                  }
                }
                return updated
              })
            }}
            fitView
          >
            <Background />
            <Controls />
            <MiniMap />
          </ReactFlow>
        </div>
      </div>
    </div>
  )
}
