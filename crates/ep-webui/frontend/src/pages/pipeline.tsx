import '@xyflow/react/dist/style.css'

import { useCallback, useEffect, useRef, useState } from 'react'
import {
  Background,
  BackgroundVariant,
  ConnectionLineType,
  Controls,
  MarkerType,
  MiniMap,
  Panel,
  ReactFlow,
  ReactFlowProvider,
  addEdge,
  useEdgesState,
  useNodesState,
  useReactFlow,
} from '@xyflow/react'
import type { Connection, Edge, OnSelectionChangeParams } from '@xyflow/react'
import { Trash2, Waypoints, X } from 'lucide-react'
import { toast } from 'sonner'

import { wsManager } from '@/api/ws'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'

import {
  BUILTIN_DEFS,
  DATA_TYPE_META,
  DRAG_MIME,
  NODE_STATUS_META,
  createPipelineNode,
  dataTypesCompatible,
  defaultParams,
  getNodePorts,
  getParamSpecs,
  moduleCapability,
  nodeKindLabel,
  normalizeNodeStatus,
  pipelineNodeTypes,
} from '@/components/shared/pipeline-node'
import type {
  DragPayload,
  NodeParams,
  ParamSpec,
  ParamValue,
  PipelineDefinition,
  PipelineFlowNode,
  PipelineNodeData,
} from '@/components/shared/pipeline-node'
import { PipelineSidebar } from '@/components/shared/pipeline-sidebar'
import { PipelineToolbar } from '@/components/shared/pipeline-toolbar'

/** 媒体查询订阅 hook（lg = 64rem 为桌面三栏布局断点） */
function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)
  useEffect(() => {
    const mql = window.matchMedia(query)
    const onChange = (event: MediaQueryListEvent) => setMatches(event.matches)
    setMatches(mql.matches)
    mql.addEventListener('change', onChange)
    return () => mql.removeEventListener('change', onChange)
  }, [query])
  return matches
}

// ============================================================
// React Flow 主题适配（映射到设计系统令牌）
// ============================================================

const RF_THEME_CSS = `
.react-flow {
  --xy-background-pattern-color: color-mix(in srgb, var(--muted-foreground) 30%, transparent);
  --xy-edge-stroke: color-mix(in srgb, var(--muted-foreground) 60%, transparent);
  --xy-edge-stroke-selected: var(--primary);
  --xy-edge-stroke-width: 1.5;
  --xy-connectionline-stroke: var(--primary);
  --xy-connectionline-stroke-width: 1.5;
  --xy-handle-background-color: var(--muted-foreground);
  --xy-handle-border-color: var(--card);
  --xy-controls-button-background-color: var(--card);
  --xy-controls-button-background-color-hover: var(--accent);
  --xy-controls-button-color: var(--muted-foreground);
  --xy-controls-button-color-hover: var(--foreground);
  --xy-controls-button-border-color: var(--border);
  --xy-controls-box-shadow: none;
  --xy-minimap-background-color: var(--card);
  --xy-minimap-node-background-color: color-mix(in srgb, var(--muted-foreground) 75%, transparent);
  --xy-minimap-mask-background-color: color-mix(in srgb, var(--muted-foreground) 45%, transparent);
  --xy-edge-label-background-color: var(--card);
  --xy-edge-label-color: var(--muted-foreground);
  --xy-selection-background-color: color-mix(in srgb, var(--primary) 10%, transparent);
  --xy-selection-border: 1px dotted color-mix(in srgb, var(--primary) 70%, transparent);
  --xy-attribution-background-color: transparent;
}
.react-flow__controls {
  border: 1px solid var(--border);
  border-radius: calc(var(--radius) - 2px);
  overflow: hidden;
}
.react-flow__controls-button {
  transition: background-color 150ms ease, color 150ms ease;
}
.react-flow__attribution a {
  color: var(--muted-foreground);
}
.react-flow__edge-path {
  transition: stroke 150ms ease;
}
`

const DEFAULT_EDGE_OPTIONS = {
  type: 'default',
  markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16 },
  style: { strokeWidth: 1.5 },
}

// ============================================================
// 示例管线（首次进入时展示完整编排效果）
// ============================================================

function examplePipeline(): { nodes: PipelineFlowNode[]; edges: Edge[] } {
  const asrCap = moduleCapability('asr')
  const llmCap = moduleCapability('llm')
  const nodes: PipelineFlowNode[] = [
    {
      id: 'demo-input',
      type: 'builtin',
      position: { x: 0, y: 170 },
      data: {
        kind: 'builtin',
        builtin: 'file_input',
        label: '文件输入',
        status: 'waiting',
        params: {
          ...defaultParams(BUILTIN_DEFS.file_input.params),
          path: '/workspace/samples/interview.wav',
        },
      },
    },
    {
      id: 'demo-asr',
      type: 'module',
      position: { x: 300, y: 50 },
      data: {
        kind: 'module',
        label: 'Paraformer 语音识别',
        moduleId: 'funasr-paraformer',
        moduleVersion: '1.0.0',
        category: 'asr',
        capabilityId: asrCap.id,
        capabilityLabel: asrCap.label,
        status: 'waiting',
        params: defaultParams(asrCap.params),
      },
    },
    {
      id: 'demo-llm',
      type: 'module',
      position: { x: 610, y: 210 },
      data: {
        kind: 'module',
        label: 'Qwen2.5 文本生成',
        moduleId: 'qwen2-7b-instruct',
        moduleVersion: '0.3.1',
        category: 'llm',
        capabilityId: llmCap.id,
        capabilityLabel: llmCap.label,
        status: 'waiting',
        params: defaultParams(llmCap.params),
      },
    },
    {
      id: 'demo-output',
      type: 'builtin',
      position: { x: 920, y: 110 },
      data: {
        kind: 'builtin',
        builtin: 'file_output',
        label: '文件输出',
        status: 'waiting',
        params: {
          ...defaultParams(BUILTIN_DEFS.file_output.params),
          path: '/workspace/output/summary.txt',
        },
      },
    },
  ]
  const edges: Edge[] = [
    {
      id: 'demo-e1',
      source: 'demo-input',
      target: 'demo-asr',
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.file.label,
    },
    {
      id: 'demo-e2',
      source: 'demo-asr',
      target: 'demo-llm',
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.text.label,
    },
    {
      id: 'demo-e3',
      source: 'demo-llm',
      target: 'demo-output',
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.text.label,
    },
  ]
  return { nodes, edges }
}

const EXAMPLE = examplePipeline()

// ============================================================
// 辅助函数
// ============================================================

function edgeLabelFor(
  sourceId: string | null | undefined,
  sourceHandle: string | null | undefined,
  nodeList: { id: string; data: PipelineNodeData }[],
): string | undefined {
  const source = nodeList.find((n) => n.id === sourceId)
  if (!source) return undefined
  const { outputs } = getNodePorts(source.data)
  const port = outputs.find((p) => p.id === (sourceHandle ?? 'out')) ?? outputs[0]
  return port ? DATA_TYPE_META[port.dataType].label : undefined
}

// ============================================================
// 右侧参数面板
// ============================================================

interface ParamFieldProps {
  spec: ParamSpec
  value: ParamValue | undefined
  onChange: (value: ParamValue) => void
}

function ParamField({ spec, value, onChange }: ParamFieldProps) {
  const numberValue = typeof value === 'number' && Number.isFinite(value) ? value : ''
  const stringValue = typeof value === 'string' ? value : value === undefined ? '' : String(value)
  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium">
          {spec.label}
          {spec.required && <span className="ml-0.5 text-status-error">*</span>}
        </span>
        {spec.hint && <span className="text-[10px] text-muted-foreground">{spec.hint}</span>}
      </div>
      {spec.type === 'string' && (
        <Input
          className="h-8 font-mono text-xs"
          value={stringValue}
          placeholder={spec.placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
      {spec.type === 'number' && (
        <Input
          type="number"
          step="any"
          className="h-8 font-mono text-xs"
          value={numberValue}
          placeholder={spec.placeholder}
          onChange={(e) => onChange(e.target.value === '' ? '' : Number(e.target.value))}
        />
      )}
      {spec.type === 'boolean' && (
        <div className="flex items-center justify-between rounded-md border border-border bg-muted/30 px-3 py-2">
          <span className="text-xs text-muted-foreground">{spec.placeholder ?? '启用'}</span>
          <Switch checked={value === true} onCheckedChange={(checked) => onChange(checked)} />
        </div>
      )}
      {spec.type === 'select' && (
        <Select value={stringValue} onValueChange={(v) => onChange(v)}>
          <SelectTrigger className="h-8 w-full text-xs">
            <SelectValue placeholder="请选择" />
          </SelectTrigger>
          <SelectContent>
            {spec.options?.map((option) => (
              <SelectItem key={option} value={option} className="text-xs">
                {option}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
    </div>
  )
}

interface NodeParamsPanelProps {
  node: PipelineFlowNode
  onParamsChange: (patch: NodeParams) => void
  onDelete: () => void
  onClose: () => void
  /** 附加布局类（窄屏 overlay 定位等） */
  className?: string
}

function NodeParamsPanel({
  node,
  onParamsChange,
  onDelete,
  onClose,
  className,
}: NodeParamsPanelProps) {
  const specs = getParamSpecs(node.data)
  const status = NODE_STATUS_META[node.data.status]
  return (
    <aside
      className={cn(
        'flex h-full w-72 shrink-0 flex-col border-l border-border bg-card',
        className,
      )}
    >
      <div className="flex shrink-0 items-start justify-between gap-2 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-semibold">{node.data.label}</p>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">
            {nodeKindLabel(node.data)}
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onClose}
          aria-label="关闭参数面板"
          title="关闭"
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      <ScrollArea className="flex-1">
        <div className="space-y-5 p-4">
          <div className="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-3 py-2">
            <span className={cn('h-2 w-2 rounded-full', status.dot)} />
            <span className="text-xs">{status.label}</span>
            <span className="ml-auto truncate font-mono text-[10px] text-muted-foreground">
              {node.id}
            </span>
          </div>

          <section className="space-y-3">
            <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              参数配置
            </h3>
            {specs.length === 0 ? (
              <p className="text-xs text-muted-foreground">此节点无可配置参数</p>
            ) : (
              specs.map((spec) => (
                <ParamField
                  key={spec.name}
                  spec={spec}
                  value={node.data.params[spec.name] ?? spec.defaultValue}
                  onChange={(v) => onParamsChange({ [spec.name]: v })}
                />
              ))
            )}
          </section>

          <section className="border-t border-border pt-4">
            <Button
              variant="outline"
              size="sm"
              className="w-full border-destructive/30 text-destructive hover:bg-destructive/10 hover:text-destructive"
              onClick={onDelete}
            >
              <Trash2 className="h-3.5 w-3.5" />
              删除节点
            </Button>
          </section>
        </div>
      </ScrollArea>
    </aside>
  )
}

// ============================================================
// 管线编辑器
// ============================================================

function PipelineEditor() {
  const { screenToFlowPosition, fitView } = useReactFlow()
  const [nodes, setNodes, onNodesChange] = useNodesState<PipelineFlowNode>(EXAMPLE.nodes)
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(EXAMPLE.edges)
  const [name, setName] = useState('示例：音频转写摘要管线')
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
  /** 窄屏（<lg）节点库抽屉开关 */
  const [libraryOpen, setLibraryOpen] = useState(false)
  const isDesktop = useMediaQuery('(min-width: 64rem)')
  const canvasRef = useRef<HTMLDivElement>(null)

  const selectedNode = selectedNodeId
    ? (nodes.find((n) => n.id === selectedNodeId) ?? null)
    : null

  // ---- 连接 ----

  const onConnect = useCallback(
    (connection: Connection) => {
      const label = edgeLabelFor(connection.source, connection.sourceHandle, nodes)
      setEdges((eds) => addEdge({ ...connection, label }, eds))
    },
    [nodes, setEdges],
  )

  const isValidConnection = useCallback(
    (connection: Connection | Edge) => {
      if (!connection.source || !connection.target || connection.source === connection.target) {
        return false
      }
      const source = nodes.find((n) => n.id === connection.source)
      const target = nodes.find((n) => n.id === connection.target)
      if (!source || !target) return false
      const outPort = getNodePorts(source.data).outputs.find(
        (p) => p.id === (connection.sourceHandle ?? 'out'),
      )
      const inPort = getNodePorts(target.data).inputs.find(
        (p) => p.id === (connection.targetHandle ?? 'in'),
      )
      if (!outPort || !inPort) return false
      return dataTypesCompatible(outPort.dataType, inPort.dataType)
    },
    [nodes],
  )

  // ---- 添加节点（拖放 / 点击节点库项） ----

  const onDragOver = useCallback((event: React.DragEvent) => {
    event.preventDefault()
    event.dataTransfer.dropEffect = 'move'
  }, [])

  /** 按载荷创建并添加节点；未指定位置时落在画布视口中央 */
  const addNodeFromPayload = useCallback(
    (payload: DragPayload, position?: { x: number; y: number }) => {
      let target = position
      if (!target) {
        const rect = canvasRef.current?.getBoundingClientRect()
        const center = rect
          ? screenToFlowPosition({
              x: rect.left + rect.width / 2,
              y: rect.top + rect.height / 2,
            })
          : { x: 112, y: 40 }
        target = center
      }
      const node = createPipelineNode(payload, { x: target.x - 112, y: target.y - 40 })
      setNodes((nds) => [...nds, node])
      toast.success(`已添加节点「${node.data.label}」`)
    },
    [screenToFlowPosition, setNodes],
  )

  const onDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault()
      const raw = event.dataTransfer.getData(DRAG_MIME)
      if (!raw) return
      try {
        const payload = JSON.parse(raw) as DragPayload
        const position = screenToFlowPosition({ x: event.clientX, y: event.clientY })
        addNodeFromPayload(payload, position)
      } catch {
        toast.error('无法添加节点', { description: '拖拽数据解析失败' })
      }
    },
    [addNodeFromPayload, screenToFlowPosition],
  )

  /** 点击节点库项：添加到画布中央；窄屏下同时收起抽屉 */
  const handleLibraryAdd = useCallback(
    (payload: DragPayload) => {
      addNodeFromPayload(payload)
      if (!isDesktop) setLibraryOpen(false)
    },
    [addNodeFromPayload, isDesktop],
  )

  // ---- 选择与参数编辑 ----

  const onSelectionChange = useCallback(({ nodes: selected }: OnSelectionChangeParams) => {
    setSelectedNodeId(selected[0]?.id ?? null)
  }, [])

  const updateNodeParams = useCallback(
    (nodeId: string, patch: NodeParams) => {
      setNodes((nds) =>
        nds.map((n) => {
          if (n.id !== nodeId) return n
          return {
            ...n,
            data: { ...n.data, params: { ...n.data.params, ...patch } },
          } as PipelineFlowNode
        }),
      )
    },
    [setNodes],
  )

  const deleteNode = useCallback(
    (nodeId: string) => {
      setNodes((nds) => nds.filter((n) => n.id !== nodeId))
      setEdges((eds) => eds.filter((e) => e.source !== nodeId && e.target !== nodeId))
      setSelectedNodeId(null)
      toast.success('节点已删除')
    },
    [setNodes, setEdges],
  )

  // ---- WebSocket 管线进度（后端就绪后实时驱动节点状态） ----

  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'progress') return
      const nodeId = typeof msg.node_id === 'string' ? msg.node_id : null
      if (!nodeId) return
      const status = normalizeNodeStatus(typeof msg.status === 'string' ? msg.status : null)
      setNodes((nds) =>
        nds.map((n) =>
          n.id === nodeId ? ({ ...n, data: { ...n.data, status } } as PipelineFlowNode) : n,
        ),
      )
    })
  }, [setNodes])

  // ---- 工具栏操作 ----

  const handleSave = useCallback(() => {
    const def: PipelineDefinition = {
      name,
      version: 1,
      nodes: nodes.map((n) => ({
        id: n.id,
        type: n.type ?? 'module',
        position: n.position,
        data: n.data,
      })),
      edges: edges.map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        sourceHandle: e.sourceHandle,
        targetHandle: e.targetHandle,
      })),
    }
    // 后端最终以 TOML 持久化；当前阶段先导出 JSON 定义文件
    const blob = new Blob([JSON.stringify(def, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = `${name.trim() || 'pipeline'}.json`
    anchor.click()
    URL.revokeObjectURL(url)
    toast.success('管线已保存', {
      description: `${def.nodes.length} 个节点 · ${def.edges.length} 条连接`,
    })
  }, [name, nodes, edges])

  const handleLoad = useCallback(
    (def: PipelineDefinition) => {
      const validTypes = new Set(['module', 'builtin', 'external'])
      const loadedNodes = def.nodes
        .filter(
          (n) =>
            n &&
            typeof n.id === 'string' &&
            validTypes.has(n.type) &&
            n.data &&
            typeof n.data === 'object' &&
            'kind' in n.data,
        )
        .map((n) => ({
          id: n.id,
          type: n.type,
          position: n.position ?? { x: 0, y: 0 },
          // 归一化状态：兼容旧定义文件中的未知状态值
          data: { ...n.data, status: normalizeNodeStatus(n.data.status) },
        })) as PipelineFlowNode[]

      const loadedEdges = def.edges.map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        sourceHandle: e.sourceHandle ?? null,
        targetHandle: e.targetHandle ?? null,
        label: edgeLabelFor(e.source, e.sourceHandle, loadedNodes),
      }))

      setNodes(loadedNodes)
      setEdges(loadedEdges as Edge[])
      setName(def.name)
      setSelectedNodeId(null)
      requestAnimationFrame(() => {
        void fitView({ padding: 0.25, duration: 300 })
      })
      toast.success('管线已加载', { description: `${loadedNodes.length} 个节点` })
    },
    [setNodes, setEdges, fitView],
  )

  const validatePipeline = useCallback((): string[] => {
    if (nodes.length === 0) return ['管线为空，请先从左侧节点库添加节点']
    const issues: string[] = []
    for (const n of nodes) {
      const { inputs, outputs } = getNodePorts(n.data)
      if (inputs.length > 0 && !edges.some((e) => e.target === n.id)) {
        issues.push(`「${n.data.label}」缺少输入连接`)
      }
      if (outputs.length > 0 && !edges.some((e) => e.source === n.id)) {
        issues.push(`「${n.data.label}」缺少输出连接`)
      }
    }
    return issues
  }, [nodes, edges])

  const handleValidate = useCallback(() => {
    const issues = validatePipeline()
    if (issues.length === 0) {
      toast.success('验证通过', {
        description: `${nodes.length} 个节点 · ${edges.length} 条连接，未发现问题`,
      })
    } else {
      toast.error(`验证发现 ${issues.length} 个问题`, {
        description: issues.slice(0, 4).join('；') + (issues.length > 4 ? '…' : ''),
      })
    }
  }, [validatePipeline, nodes.length, edges.length])

  const handleExecute = useCallback(() => {
    const issues = validatePipeline()
    if (issues.length > 0) {
      toast.error('管线验证未通过', { description: issues[0] })
      return
    }
    // TODO: 后端就绪后改为 POST /api/pipelines/execute
    toast.info('管线执行功能开发中')
  }, [validatePipeline])

  // ---- 渲染 ----

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <style>{RF_THEME_CSS}</style>
      <PipelineToolbar
        name={name}
        onNameChange={setName}
        nodeCount={nodes.length}
        edgeCount={edges.length}
        libraryOpen={libraryOpen}
        onToggleLibrary={() => setLibraryOpen((open) => !open)}
        onSave={handleSave}
        onLoad={handleLoad}
        onValidate={handleValidate}
        onExecute={handleExecute}
      />

      <div className="relative flex min-h-0 flex-1 overflow-hidden">
        {/* 桌面（≥lg）：节点库常驻左栏；窄屏：抽屉式 overlay */}
        {isDesktop ? (
          <PipelineSidebar onAdd={handleLibraryAdd} />
        ) : (
          libraryOpen && (
            <PipelineSidebar
              onAdd={handleLibraryAdd}
              onClose={() => setLibraryOpen(false)}
              className="absolute inset-y-0 left-0 z-40 shadow-lg"
            />
          )
        )}

        <div ref={canvasRef} className="relative min-w-0 flex-1">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            nodeTypes={pipelineNodeTypes}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            isValidConnection={isValidConnection}
            onDrop={onDrop}
            onDragOver={onDragOver}
            onSelectionChange={onSelectionChange}
            defaultEdgeOptions={DEFAULT_EDGE_OPTIONS}
            connectionLineType={ConnectionLineType.Bezier}
            connectionRadius={32}
            deleteKeyCode={['Backspace', 'Delete']}
            fitView
            fitViewOptions={{ padding: 0.25 }}
            minZoom={0.2}
            maxZoom={2}
          >
            <Background variant={BackgroundVariant.Dots} gap={22} size={1.5} />
            <Controls position="bottom-left" showInteractive={false} />
            <MiniMap pannable zoomable nodeStrokeWidth={2} />
            <Panel position="bottom-center" className="pointer-events-none">
              <div className="hidden max-w-[28rem] flex-wrap items-center justify-center gap-x-3 gap-y-1 rounded-lg border border-border bg-card/85 px-3.5 py-1.5 shadow-md backdrop-blur md:flex">
                {Object.entries(NODE_STATUS_META).map(([key, meta]) => (
                  <span
                    key={key}
                    className="flex items-center gap-1.5 text-[11px] text-muted-foreground"
                  >
                    <span className={cn('h-2 w-2 rounded-full', meta.dot)} />
                    {meta.label}
                  </span>
                ))}
              </div>
            </Panel>
          </ReactFlow>

          {nodes.length === 0 && (
            <div className="pointer-events-none absolute inset-0 z-10 flex flex-col items-center justify-center gap-3">
              <span className="flex h-14 w-14 items-center justify-center rounded-2xl border border-dashed border-border bg-card/60">
                <Waypoints className="h-6 w-6 text-muted-foreground" />
              </span>
              <p className="text-sm font-medium">画布还是空的</p>
              <p className="text-xs text-muted-foreground">
                从左侧节点库点击或拖入模块 / 内置节点，开始编排你的第一条管线
              </p>
            </div>
          )}
        </div>

        {/* 桌面（≥lg）：参数面板常驻右栏；窄屏：右侧 overlay */}
        {selectedNode && (
          <NodeParamsPanel
            node={selectedNode}
            onParamsChange={(patch) => updateNodeParams(selectedNode.id, patch)}
            onDelete={() => deleteNode(selectedNode.id)}
            onClose={() => setSelectedNodeId(null)}
            className={
              isDesktop ? undefined : 'absolute inset-y-0 right-0 z-30 max-w-[85vw] shadow-lg'
            }
          />
        )}
      </div>
    </div>
  )
}

export function PipelinePage() {
  return (
    <ReactFlowProvider>
      <PipelineEditor />
    </ReactFlowProvider>
  )
}
