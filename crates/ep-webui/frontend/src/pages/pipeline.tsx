import '@xyflow/react/dist/style.css'

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
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
import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import {
  ChevronDown,
  CircleCheck,
  Copy,
  FolderOpen,
  Loader2,
  Plus,
  RefreshCw,
  Sparkles,
  Trash2,
  Waypoints,
  X,
} from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type {
  ExecutePipelineRequest,
  PipelineEdgeSpec,
  PipelineNodeSpec,
  PipelineSpec,
  PipelineSummary,
} from '@/api/types'
import { wsManager } from '@/api/ws'
import i18n from '@/i18n'
import { cn } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
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
  BuiltinKind,
  DragPayload,
  NodeParams,
  ParamSpec,
  ParamValue,
  PipelineDefinition,
  PipelineFlowNode,
  PipelineNodeData,
} from '@/components/shared/pipeline-node'
import { confirmDialog } from '@/components/shared/confirm-dialog'
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

/**
 * 模块级（非组件上下文）取 pipeline 命名空间文案。
 * 用于模块顶层执行的 examplePipeline / 纯函数 toSpec 等无法调用 hook 的位置。
 */
function tp(key: string, options?: Record<string, unknown>): string {
  return i18n.t(key, { ns: 'pipeline', ...(options ?? {}) })
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
// 示例管线（本地模板：服务端列表为空时可一键载入，不再作为默认画布）
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
        label: tp('template.exampleFileInput'),
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
        label: tp('template.exampleAsrNode'),
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
        label: tp('template.exampleLlmNode'),
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
        label: tp('template.exampleFileOutput'),
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

/** 后端错误对象 → 可展示文案 */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

// ============================================================
// spec ↔ React Flow 互转（纯函数）
// ============================================================

/** 管线 id 命名规则（与后端 PUT /api/pipelines/:id 校验一致） */
const PIPELINE_ID_RULE = /^[a-z0-9][a-z0-9-]*$/

interface CanvasMeta {
  id: string
  name: string
  description: string
}

/** 深度排序对象键后序列化，保证相同内容产生相同指纹（与键插入顺序无关） */
function canonicalJson(value: unknown): string {
  const sort = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(sort)
    if (v && typeof v === 'object') {
      const out: Record<string, unknown> = {}
      for (const key of Object.keys(v as Record<string, unknown>).sort()) {
        out[key] = sort((v as Record<string, unknown>)[key])
      }
      return out
    }
    return v
  }
  return JSON.stringify(sort(value))
}

/**
 * React Flow 画布 → 服务端 PipelineSpec（toSpec）。
 *
 * - 节点保留 id/label/kind/builtin/module_id/capability/params/position；
 *   params 整体原样输出（含 UI 不展示的字段，往返不丢失）。
 * - 边以 `{from: [node, port], to: [node, port]}` 输出，sourceHandle/targetHandle
 *   即节点端口 id（缺省回退 out/in，与现有端口定义一致）。
 * - 外部 API 节点不在服务端 spec 契约内（后端仅接受 builtin/module），
 *   调用方需先拦截；此处作为最后防线抛错。
 */
function toSpec(
  nodes: PipelineFlowNode[],
  edges: Edge[],
  meta: CanvasMeta,
): PipelineSpec {
  const specNodes: PipelineNodeSpec[] = nodes.map((n): PipelineNodeSpec => {
    const common = {
      id: n.id,
      label: n.data.label,
      params: { ...n.data.params },
      position: { x: n.position.x, y: n.position.y },
    }
    switch (n.data.kind) {
      case 'builtin':
        return { ...common, kind: 'builtin', builtin: n.data.builtin }
      case 'module':
        return {
          ...common,
          kind: 'module',
          module_id: n.data.moduleId,
          capability: n.data.capabilityId,
        }
      case 'external':
        throw new Error(tp('save.externalNodeRejected', { label: n.data.label }))
    }
  })
  const specEdges: PipelineEdgeSpec[] = edges.map((e) => ({
    from: [e.source, e.sourceHandle ?? 'out'],
    to: [e.target, e.targetHandle ?? 'in'],
  }))
  return {
    pipeline: { id: meta.id, name: meta.name, description: meta.description },
    nodes: specNodes,
    edges: specEdges,
  }
}

/** capability → 模块分类（取 `.` 前缀，如 `asr.transcribe` → `asr`） */
function categoryFromCapability(capability: string | undefined): string {
  const prefix = (capability ?? '').split('.')[0].trim()
  return prefix || 'other'
}

/**
 * 简单级联布局：按拓扑深度分列（Kahn 求最长路径深度），每列纵向排开。
 * 仅用于 spec 中缺少 position 的节点；环上节点保持深度 0。
 */
function cascadeLayout(spec: PipelineSpec): Map<string, { x: number; y: number }> {
  const ids = spec.nodes.map((n) => n.id)
  const depth = new Map<string, number>(ids.map((id) => [id, 0]))
  const indegree = new Map<string, number>(ids.map((id) => [id, 0]))
  const children = new Map<string, Set<string>>()
  for (const e of spec.edges ?? []) {
    if (!Array.isArray(e.from) || !Array.isArray(e.to)) continue
    const u = e.from[0]
    const v = e.to[0]
    if (!depth.has(u) || !depth.has(v) || u === v) continue
    const set = children.get(u) ?? new Set<string>()
    if (!set.has(v)) {
      set.add(v)
      children.set(u, set)
      indegree.set(v, (indegree.get(v) ?? 0) + 1)
    }
  }
  const queue = ids.filter((id) => (indegree.get(id) ?? 0) === 0)
  while (queue.length > 0) {
    const u = queue.shift()!
    for (const v of children.get(u) ?? []) {
      depth.set(v, Math.max(depth.get(v) ?? 0, (depth.get(u) ?? 0) + 1))
      indegree.set(v, (indegree.get(v) ?? 0) - 1)
      if (indegree.get(v) === 0) queue.push(v)
    }
  }
  const columns = new Map<number, string[]>()
  for (const id of ids) {
    const col = depth.get(id) ?? 0
    const list = columns.get(col) ?? []
    list.push(id)
    columns.set(col, list)
  }
  const positions = new Map<string, { x: number; y: number }>()
  for (const [col, list] of columns) {
    list.forEach((id, row) => {
      positions.set(id, { x: 40 + col * 320, y: 40 + row * 160 })
    })
  }
  return positions
}

/**
 * 端口名归一：服务端 spec 的端口名（input/output，ep-core 契约）
 * ↔ 前端节点 handle id（in/out，BUILTIN_DEFS 定义）。
 * fallback 为对应方向的默认 handle。
 */
function normalizePortName(
  port: string | undefined,
  fallback: 'in' | 'out',
): string {
  if (!port) return fallback
  if (port === 'input' || port === 'in') return 'in'
  if (port === 'output' || port === 'out') return 'out'
  return port
}

/**
 * 服务端 PipelineSpec → React Flow 画布（fromSpec）。
 *
 * - builtin 节点：按 builtin 名恢复（未知 builtin 跳过并计数）；
 * - module 节点：category 由 capability 前缀推导，参数模式随之恢复；
 * - params 原样保留（含 UI 不展示字段，往返不丢失）；
 * - position 缺失的节点走级联布局；
 * - 引用未知节点的边被过滤。
 */
function fromSpec(spec: PipelineSpec): {
  nodes: PipelineFlowNode[]
  edges: Edge[]
  skippedNodes: number
} {
  const layout = cascadeLayout(spec)
  const nodes: PipelineFlowNode[] = []
  const known = new Set<string>()
  let skippedNodes = 0

  for (const sn of spec.nodes ?? []) {
    const position = sn.position ?? layout.get(sn.id) ?? { x: 0, y: 0 }
    // 保留非 UI 展示的参数值：运行时原样存取，toSpec 时整体写回
    const params = (sn.params ?? {}) as NodeParams

    if (sn.kind === 'builtin') {
      const builtin = sn.builtin as BuiltinKind | undefined
      if (builtin && builtin in BUILTIN_DEFS) {
        nodes.push({
          id: sn.id,
          type: 'builtin',
          position,
          data: {
            kind: 'builtin',
            builtin,
            label: sn.label || BUILTIN_DEFS[builtin].label,
            status: 'waiting',
            params,
          },
        })
        known.add(sn.id)
        continue
      }
      skippedNodes += 1
      continue
    }

    if (sn.kind === 'module') {
      const category = categoryFromCapability(sn.capability)
      const cap = moduleCapability(category)
      nodes.push({
        id: sn.id,
        type: 'module',
        position,
        data: {
          kind: 'module',
          label: sn.label || cap.label,
          moduleId: sn.module_id || 'unknown',
          // spec 契约不含版本，仅卡片展示用
          moduleVersion: '1.0.0',
          category,
          capabilityId: sn.capability || cap.id,
          capabilityLabel: cap.label,
          status: 'waiting',
          params,
        },
      })
      known.add(sn.id)
      continue
    }

    // 未知 kind（如 external_api 不在前端 spec 契约内）
    skippedNodes += 1
  }

  const edges: Edge[] = (spec.edges ?? [])
    .filter(
      (e) =>
        Array.isArray(e.from) &&
        Array.isArray(e.to) &&
        known.has(e.from[0]) &&
        known.has(e.to[0]),
    )
    .map((e, index) => ({
      id: `e-${e.from[0]}:${e.from[1]}->${e.to[0]}:${e.to[1]}-${index}`,
      source: e.from[0],
      // 服务端 spec 端口名（input/output）归一到前端 handle id（in/out），
      // 否则内置管线的边会因 handle 不存在被 React Flow 静默丢弃
      sourceHandle: normalizePortName(e.from[1], 'out'),
      target: e.to[0],
      targetHandle: normalizePortName(e.to[1], 'in'),
      label: edgeLabelFor(e.from[0], e.from[1], nodes),
    }))

  return { nodes, edges, skippedNodes }
}

/**
 * 画布指纹（忽略节点运行状态 status）：用于判断相对上次加载 / 保存
 * 是否存在未保存更改。meta.id 变化（如另存为）同样视为内容变化的一部分。
 */
function canvasFingerprint(
  nodes: PipelineFlowNode[],
  edges: Edge[],
  meta: CanvasMeta,
): string {
  return canonicalJson({
    meta,
    nodes: nodes.map((n) => ({
      id: n.id,
      type: n.type ?? '',
      position: n.position,
      data: { ...n.data, status: 'waiting' },
    })),
    edges: edges.map((e) => ({
      source: e.source,
      sourceHandle: e.sourceHandle ?? null,
      target: e.target,
      targetHandle: e.targetHandle ?? null,
    })),
  })
}

/** 新建空白管线最小模板：文件输入 → 文件输出 */
function blankTemplate(): { nodes: PipelineFlowNode[]; edges: Edge[] } {
  const input = createPipelineNode(
    { nodeType: 'builtin', builtin: 'file_input' },
    { x: 40, y: 100 },
  )
  const output = createPipelineNode(
    { nodeType: 'builtin', builtin: 'file_output' },
    { x: 420, y: 100 },
  )
  const edges: Edge[] = [
    {
      id: `e-${input.id}->${output.id}`,
      source: input.id,
      target: output.id,
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.file.label,
    },
  ]
  return { nodes: [input, output], edges }
}

/** 未保存画布按 spec 执行时的兜底 pipeline.id（后端要求非空） */
function fallbackPipelineId(name: string): string {
  const slug = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
  return slug || `pipeline-${Date.now().toString(36)}`
}

// ============================================================
// 必填参数校验（修复 P1-21：执行前不得放过空的必填参数）
// ============================================================

interface MissingRequiredParam {
  nodeId: string
  nodeLabel: string
  spec: ParamSpec
  current: ParamValue | undefined
}

/** 空值定义：未设置 / 空字符串（含纯空白）。数字 0、布尔 false 不算空。 */
function isParamEmpty(value: ParamValue | undefined): boolean {
  return value === undefined || (typeof value === 'string' && value.trim() === '')
}

/** 遍历所有节点的参数模式，收集 required=true 且当前为空的参数 */
function collectMissingRequired(nodes: PipelineFlowNode[]): MissingRequiredParam[] {
  const missing: MissingRequiredParam[] = []
  for (const n of nodes) {
    for (const spec of getParamSpecs(n.data)) {
      if (!spec.required) continue
      const value = n.data.params[spec.name]
      if (isParamEmpty(value)) {
        missing.push({ nodeId: n.id, nodeLabel: n.data.label, spec, current: value })
      }
    }
  }
  return missing
}

// ============================================================
// 执行对话框字段
// ============================================================

interface ExecField {
  /** `${nodeId}:${paramName}` */
  key: string
  nodeId: string
  nodeLabel: string
  spec: ParamSpec
  current: ParamValue | undefined
  /** file_input 的 path：每次执行注入的服务器文件路径 */
  isInputPath: boolean
}

/**
 * 构建执行对话框字段：
 * 1) 每个 file_input 节点的 path 始终收集（预填当前值，可在执行时覆盖）；
 * 2) 其余为空的必填参数在此补齐。
 */
function buildExecFields(nodes: PipelineFlowNode[]): ExecField[] {
  const fields: ExecField[] = []
  const seen = new Set<string>()
  for (const n of nodes) {
    if (n.data.kind === 'builtin' && n.data.builtin === 'file_input') {
      const pathSpec = BUILTIN_DEFS.file_input.params.find((p) => p.name === 'path')
      if (!pathSpec) continue
      const key = `${n.id}:path`
      fields.push({
        key,
        nodeId: n.id,
        nodeLabel: n.data.label,
        spec: pathSpec,
        current: n.data.params.path,
        isInputPath: true,
      })
      seen.add(key)
    }
  }
  for (const m of collectMissingRequired(nodes)) {
    const key = `${m.nodeId}:${m.spec.name}`
    if (seen.has(key)) continue
    seen.add(key)
    fields.push({
      key,
      nodeId: m.nodeId,
      nodeLabel: m.nodeLabel,
      spec: m.spec,
      current: m.current,
      isInputPath: false,
    })
  }
  return fields
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
  const { t } = useTranslation('pipeline')
  const numberValue = typeof value === 'number' && Number.isFinite(value) ? value : ''
  const stringValue = typeof value === 'string' ? value : value === undefined ? '' : String(value)
  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium">
          {spec.label}
          {spec.required && (
            <span className="ml-0.5 text-status-error" aria-label={t('common:label.required')}>
              *
            </span>
          )}
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
          <span className="text-xs text-muted-foreground">
            {spec.placeholder ?? t('params.enable')}
          </span>
          <Switch checked={value === true} onCheckedChange={(checked) => onChange(checked)} />
        </div>
      )}
      {spec.type === 'select' && (
        <Select value={stringValue} onValueChange={(v) => onChange(v)}>
          <SelectTrigger className="h-8 w-full text-xs">
            <SelectValue placeholder={t('params.selectPlaceholder')} />
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
  const { t } = useTranslation('pipeline')
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
          aria-label={t('nodePanel.closeAria')}
          title={t('common:action.close')}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      <ScrollArea className="flex-1">
        <div className="space-y-5 p-4">
          <div className="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-3 py-2">
            <span className={cn('h-2 w-2 rounded-full', status.dot)} />
            <span className="text-xs">{t(`nodeStatus.${node.data.status}`)}</span>
            <span className="ml-auto truncate font-mono text-[10px] text-muted-foreground">
              {node.id}
            </span>
          </div>

          <section className="space-y-3">
            <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              {t('nodePanel.paramsTitle')}
            </h3>
            {specs.length === 0 ? (
              <p className="text-xs text-muted-foreground">{t('nodePanel.noParams')}</p>
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
              {t('nodePanel.deleteNode')}
            </Button>
          </section>
        </div>
      </ScrollArea>
    </aside>
  )
}

// ============================================================
// 管线库工具条（服务端持久化入口：库下拉 + 状态 + 新建 / 另存为 / 删除）
// ============================================================

interface PipelineLibraryBarProps {
  pipelines: PipelineSummary[] | null
  error: boolean
  onRefresh: () => void
  currentId: string | null
  currentSource: 'builtin' | 'custom' | null
  dirty: boolean
  executing: boolean
  onSelect: (id: string) => void
  onNewBlank: () => void
  onLoadExample: () => void
  onSaveAs: () => void
  onDelete: () => void
}

function SourceBadge({ source }: { source: 'builtin' | 'custom' }) {
  const { t } = useTranslation('pipeline')
  return (
    <Badge
      variant={source === 'builtin' ? 'secondary' : 'outline'}
      className="h-4 shrink-0 px-1 text-[9px]"
    >
      {source === 'builtin' ? t('source.builtin') : t('source.custom')}
    </Badge>
  )
}

function PipelineLibraryBar({
  pipelines,
  error,
  onRefresh,
  currentId,
  currentSource,
  dirty,
  executing,
  onSelect,
  onNewBlank,
  onLoadExample,
  onSaveAs,
  onDelete,
}: PipelineLibraryBarProps) {
  const { t } = useTranslation('pipeline')
  return (
    <div className="flex h-11 shrink-0 items-center gap-2 overflow-x-auto border-b border-border bg-muted/30 px-3">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm" className="shrink-0" title={t('library.title')}>
            <FolderOpen className="h-3.5 w-3.5" />
            {t('library.label')}
            {pipelines === null && !error ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              pipelines !== null && (
                <span className="rounded-full bg-muted px-1.5 font-mono text-[10px] text-muted-foreground">
                  {pipelines.length}
                </span>
              )
            )}
            <ChevronDown className="h-3 w-3 text-muted-foreground" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-h-96 w-80 overflow-y-auto">
          <DropdownMenuLabel>{t('library.serverPipelines')}</DropdownMenuLabel>
          {error && (
            <DropdownMenuItem onSelect={onRefresh}>
              <span className="text-status-error">{t('library.loadFailedRetry')}</span>
            </DropdownMenuItem>
          )}
          {!error && pipelines === null && (
            <DropdownMenuItem disabled>{t('library.loading')}</DropdownMenuItem>
          )}
          {!error && pipelines !== null && pipelines.length === 0 && (
            <DropdownMenuItem onSelect={onLoadExample}>
              <span>{t('library.emptyLoadExample')}</span>
            </DropdownMenuItem>
          )}
          {!error &&
            pipelines?.map((p) => (
              <DropdownMenuItem
                key={p.id}
                onSelect={() => onSelect(p.id)}
                className="items-start gap-2"
                title={t('library.loadPipeline', { name: p.name })}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-[13px]">{p.name}</span>
                    <SourceBadge source={p.source} />
                    {p.id === currentId && (
                      <CircleCheck
                        className="h-3 w-3 shrink-0 text-primary"
                        aria-label={t('library.currentPipeline')}
                      />
                    )}
                  </div>
                  {p.description && (
                    <p className="truncate text-[11px] text-muted-foreground">{p.description}</p>
                  )}
                  <p className="truncate font-mono text-[10px] text-muted-foreground">{p.id}</p>
                </div>
              </DropdownMenuItem>
            ))}
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={onNewBlank}>
            <Plus className="h-3.5 w-3.5" />
            {t('library.newBlank')}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onLoadExample}>
            <Sparkles className="h-3.5 w-3.5" />
            {t('library.loadExample')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        {currentId ? (
          <>
            <span className="max-w-40 truncate font-mono text-[11px]" title={currentId}>
              {currentId}
            </span>
            {currentSource && <SourceBadge source={currentSource} />}
          </>
        ) : (
          <span className="whitespace-nowrap">{t('library.notSaved')}</span>
        )}
        {dirty && (
          <span className="whitespace-nowrap text-status-starting">
            {t('library.unsavedChanges')}
          </span>
        )}
        {executing && (
          <span className="flex items-center gap-1 whitespace-nowrap text-status-starting">
            <Loader2 className="h-3 w-3 animate-spin" />
            {t('library.executing')}
          </span>
        )}
      </div>

      <div className="ml-auto flex shrink-0 items-center gap-1">
        <Button
          variant="ghost"
          size="xs"
          onClick={onNewBlank}
          title={t('library.newBlankHint')}
        >
          <Plus className="h-3.5 w-3.5" />
          {t('library.new')}
        </Button>
        <Button variant="ghost" size="xs" onClick={onSaveAs} title={t('library.saveAsHint')}>
          <Copy className="h-3.5 w-3.5" />
          {t('library.saveAs')}
        </Button>
        {/* 内置管线不显示删除按钮（后端亦会 403 拒绝） */}
        {currentId && currentSource === 'custom' && (
          <Button
            variant="ghost"
            size="xs"
            className="text-destructive hover:bg-destructive/10 hover:text-destructive"
            onClick={onDelete}
            title={t('library.deleteHint')}
          >
            <Trash2 className="h-3.5 w-3.5" />
            {t('common:action.delete')}
          </Button>
        )}
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onRefresh}
          title={t('library.refreshList')}
          aria-label={t('library.refreshList')}
        >
          <RefreshCw className="h-3 w-3" />
        </Button>
      </div>
    </div>
  )
}

// ============================================================
// 另存为对话框
// ============================================================

interface SaveAsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  defaultName: string
  pipelines: PipelineSummary[] | null
  /** 返回是否保存成功（失败时保持打开以便重试） */
  onConfirm: (name: string, id: string) => Promise<boolean>
}

function SaveAsDialog({ open, onOpenChange, defaultName, pipelines, onConfirm }: SaveAsDialogProps) {
  const { t } = useTranslation('pipeline')
  const [name, setName] = useState('')
  const [id, setId] = useState('')
  const [attempted, setAttempted] = useState(false)
  const [pending, setPending] = useState(false)

  useEffect(() => {
    if (open) {
      setName(defaultName)
      setId('')
      setAttempted(false)
      setPending(false)
    }
  }, [open, defaultName])

  const nameOk = name.trim().length > 0
  const idValid = PIPELINE_ID_RULE.test(id)
  const conflict = idValid ? (pipelines?.find((p) => p.id === id) ?? null) : null

  const handleSubmit = async () => {
    setAttempted(true)
    if (!nameOk || !idValid || pending) return
    setPending(true)
    try {
      const ok = await onConfirm(name.trim(), id)
      if (ok) onOpenChange(false)
    } finally {
      setPending(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !pending && onOpenChange(next)}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t('saveAs.title')}</DialogTitle>
          <DialogDescription>{t('saveAs.description')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-1">
          <div className="space-y-1.5">
            <span className="text-xs font-medium">
              {t('saveAs.nameLabel')}
              <span className="ml-0.5 text-status-error" aria-label={t('common:label.required')}>
                *
              </span>
            </span>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t('saveAs.namePlaceholder')}
              className="h-8 text-sm"
              autoFocus
            />
            {attempted && !nameOk && (
              <p className="text-[11px] text-status-error">{t('saveAs.nameRequired')}</p>
            )}
          </div>
          <div className="space-y-1.5">
            <span className="text-xs font-medium">
              {t('saveAs.idLabel')}
              <span className="ml-0.5 text-status-error" aria-label={t('common:label.required')}>
                *
              </span>
            </span>
            <Input
              value={id}
              onChange={(e) => setId(e.target.value)}
              placeholder="my-pipeline-1"
              className="h-8 font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">{t('saveAs.idHint')}</p>
            {attempted && !idValid && (
              <p className="text-[11px] text-status-error">
                {id ? t('saveAs.idInvalid', { id }) : t('saveAs.idEmpty')}
              </p>
            )}
            {conflict && (
              <p className="text-[11px] text-status-starting">
                {t('saveAs.idConflict', {
                  source: conflict.source === 'builtin' ? t('source.builtin') : t('source.custom'),
                })}
              </p>
            )}
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            {t('common:action.cancel')}
          </Button>
          <Button onClick={handleSubmit} disabled={pending}>
            {pending && <Loader2 className="animate-spin" />}
            {t('common:action.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ============================================================
// 执行对话框（收集 file_input 路径 + 补齐其他空必填参数）
// ============================================================

interface ExecuteDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  fields: ExecField[]
  submitting: boolean
  /** values: key（`${nodeId}:${paramName}`）→ 参数值 */
  onSubmit: (values: Record<string, ParamValue>) => void
}

function ExecuteDialog({ open, onOpenChange, fields, submitting, onSubmit }: ExecuteDialogProps) {
  const { t } = useTranslation('pipeline')
  const [values, setValues] = useState<Record<string, ParamValue>>({})
  const [attempted, setAttempted] = useState(false)

  useEffect(() => {
    if (open) {
      const init: Record<string, ParamValue> = {}
      for (const f of fields) {
        if (f.current !== undefined && !isParamEmpty(f.current)) init[f.key] = f.current
      }
      setValues(init)
      setAttempted(false)
    }
  }, [open, fields])

  // 按节点分组展示（保持 fields 原顺序）
  const groups = useMemo(() => {
    const list: { nodeId: string; label: string; fields: ExecField[] }[] = []
    for (const f of fields) {
      const group = list.find((g) => g.nodeId === f.nodeId)
      if (group) {
        group.fields.push(f)
      } else {
        list.push({ nodeId: f.nodeId, label: f.nodeLabel, fields: [f] })
      }
    }
    return list
  }, [fields])

  const emptyCount = fields.filter((f) => isParamEmpty(values[f.key])).length

  const handleSubmit = () => {
    setAttempted(true)
    if (emptyCount > 0) {
      toast.error(t('execute.missingRequired', { count: emptyCount }))
      return
    }
    onSubmit(values)
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !submitting && onOpenChange(next)}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('execute.title')}</DialogTitle>
          <DialogDescription>{t('execute.description')}</DialogDescription>
        </DialogHeader>
        <ScrollArea className="max-h-[50vh]">
          <div className="space-y-5 p-1 pr-3">
            {groups.map((group) => (
              <section key={group.nodeId} className="space-y-3">
                <div className="flex items-baseline gap-2">
                  <h3 className="text-xs font-semibold">{group.label}</h3>
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {group.nodeId}
                  </span>
                </div>
                {group.fields.map((f) => {
                  const empty = attempted && isParamEmpty(values[f.key])
                  return (
                    <div
                      key={f.key}
                      className={cn(
                        'rounded-md p-2',
                        f.isInputPath && 'border border-border bg-muted/30',
                      )}
                    >
                      {f.isInputPath && (
                        <p className="mb-1.5 text-[10px] text-muted-foreground">
                          {t('execute.inputPathHint')}
                        </p>
                      )}
                      <ParamField
                        spec={f.spec}
                        value={values[f.key]}
                        onChange={(v) => setValues((prev) => ({ ...prev, [f.key]: v }))}
                      />
                      {empty && (
                        <p className="mt-1 text-[11px] text-status-error">
                          {t('execute.requiredHint')}
                        </p>
                      )}
                    </div>
                  )
                })}
              </section>
            ))}
            {fields.length === 0 && (
              <p className="text-xs text-muted-foreground">{t('execute.noFields')}</p>
            )}
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={submitting}>
            {t('common:action.cancel')}
          </Button>
          <Button onClick={handleSubmit} disabled={submitting}>
            {submitting && <Loader2 className="animate-spin" />}
            {t('execute.submit')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ============================================================
// 管线编辑器
// ============================================================

function PipelineEditor() {
  const { t } = useTranslation('pipeline')
  const { screenToFlowPosition, fitView } = useReactFlow()
  const [nodes, setNodes, onNodesChange] = useNodesState<PipelineFlowNode>([])
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([])
  const [name, setName] = useState(t('canvas.untitledName'))
  const [description, setDescription] = useState('')
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
  /** 窄屏（<lg）节点库抽屉开关 */
  const [libraryOpen, setLibraryOpen] = useState(false)
  const isDesktop = useMediaQuery('(min-width: 64rem)')
  const canvasRef = useRef<HTMLDivElement>(null)

  // ---- 服务端管线库状态 ----
  const [pipelines, setPipelines] = useState<PipelineSummary[] | null>(null)
  const [libraryError, setLibraryError] = useState(false)
  /** 当前画布对应的服务端管线 id（null = 未保存的本地画布） */
  const [currentId, setCurrentId] = useState<string | null>(null)
  const [currentSource, setCurrentSource] = useState<'builtin' | 'custom' | null>(null)
  /** 上次加载 / 保存时的画布指纹，用于判定未保存更改 */
  const [baseline, setBaseline] = useState(() =>
    canvasFingerprint([], [], { id: '', name: t('canvas.untitledName'), description: '' }),
  )
  const [saveAsOpen, setSaveAsOpen] = useState(false)

  // ---- 执行状态 ----
  // 执行中禁用再次执行（同一画布）。解锁依赖 WS progress：任一节点 failed
  // 或全部节点到达终态。若后端始终未推送进度（如 daemon 未重启），锁保持到
  // 页面刷新——宁可保守也不允许重复提交。
  const [executing, setExecuting] = useState(false)
  const executingRef = useRef(false)
  const [execDialogOpen, setExecDialogOpen] = useState(false)
  const [execFields, setExecFields] = useState<ExecField[]>([])
  const [execSubmitting, setExecSubmitting] = useState(false)

  const meta = useMemo<CanvasMeta>(
    () => ({ id: currentId ?? '', name, description }),
    [currentId, name, description],
  )
  const dirty = useMemo(
    () => canvasFingerprint(nodes, edges, meta) !== baseline,
    [nodes, edges, meta, baseline],
  )

  // WS 回调在订阅时闭包固定，用 ref 读取最新画布 / 执行态
  const nodesRef = useRef<PipelineFlowNode[]>([])
  useEffect(() => {
    nodesRef.current = nodes
  }, [nodes])

  const selectedNode = selectedNodeId
    ? (nodes.find((n) => n.id === selectedNodeId) ?? null)
    : null

  const fitSoon = useCallback(() => {
    requestAnimationFrame(() => {
      void fitView({ padding: 0.25, duration: 300 })
    })
  }, [fitView])

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
      toast.success(t('node.addedToast', { label: node.data.label }))
    },
    [screenToFlowPosition, setNodes, t],
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
        toast.error(t('node.addFailed'), { description: t('node.dragParseFailed') })
      }
    },
    [addNodeFromPayload, screenToFlowPosition, t],
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
      toast.success(t('node.deletedToast'))
    },
    [setNodes, setEdges, t],
  )

  /**
   * 删除节点确认（修复 P2-26）。两条路径：
   * 1) 参数面板「删除节点」按钮 → 本函数确认后调用 deleteNode；
   * 2) 键盘 Delete/Backspace 等 React Flow 内置删除路径 → 由下方 ReactFlow
   *    的 onBeforeDelete 内联回调拦截确认。
   */
  const requestDeleteNode = useCallback(
    async (nodeId: string) => {
      const target = nodes.find((n) => n.id === nodeId)
      const ok = await confirmDialog({
        title: t('node.deleteTitle'),
        description: t('node.deleteDescriptionSingle', {
          label: target?.data.label ?? nodeId,
        }),
        confirmLabel: t('common:action.delete'),
        cancelLabel: t('common:action.cancel'),
        variant: 'destructive',
      })
      if (ok) deleteNode(nodeId)
    },
    [nodes, deleteNode, t],
  )

  // ---- WebSocket 管线进度（实时驱动节点状态） ----

  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'progress') return
      const nodeId = typeof msg.node_id === 'string' ? msg.node_id : null
      if (!nodeId) return
      const current = nodesRef.current
      if (!current.some((n) => n.id === nodeId)) return
      const status = normalizeNodeStatus(typeof msg.status === 'string' ? msg.status : null)
      const next = current.map((n) =>
        n.id === nodeId ? ({ ...n, data: { ...n.data, status } } as PipelineFlowNode) : n,
      )
      setNodes(next)
      // 执行结束判定：任一节点失败（管线中止）或全部节点到达终态 → 解锁执行按钮
      if (
        executingRef.current &&
        (status === 'failed' ||
          next.every((n) => n.data.status === 'done' || n.data.status === 'failed'))
      ) {
        executingRef.current = false
        setExecuting(false)
      }
    })
  }, [setNodes])

  // ---- 服务端管线库 ----

  const refreshPipelines = useCallback(() => {
    api
      .listPipelines()
      .then((list) => {
        setPipelines(list)
        setLibraryError(false)
      })
      .catch(() => setLibraryError(true))
  }, [])

  useEffect(() => {
    refreshPipelines()
  }, [refreshPipelines])

  /** 有未保存更改时先确认（将丢弃未保存更改） */
  const confirmDiscardIfDirty = useCallback(async (): Promise<boolean> => {
    if (!dirty) return true
    return confirmDialog({
      title: t('discard.title'),
      description: t('discard.description'),
      confirmLabel: t('discard.confirm'),
      cancelLabel: t('common:action.cancel'),
    })
  }, [dirty, t])

  /** 重置为空白画布（未保存到服务端状态） */
  const resetCanvas = useCallback(
    (
      nextNodes: PipelineFlowNode[],
      nextEdges: Edge[],
      nextName: string,
      nextDescription: string,
    ) => {
      const nextMeta = { id: '', name: nextName, description: nextDescription }
      setNodes(nextNodes)
      setEdges(nextEdges)
      setName(nextName)
      setDescription(nextDescription)
      setCurrentId(null)
      setCurrentSource(null)
      setSelectedNodeId(null)
      setBaseline(canvasFingerprint(nextNodes, nextEdges, nextMeta))
    },
    [setNodes, setEdges],
  )

  /** 选择服务端管线 → 加载 spec 并铺到画布 */
  const handleSelectPipeline = useCallback(
    async (id: string) => {
      if (!(await confirmDiscardIfDirty())) return
      const toastId = toast.loading(t('load.loading'))
      try {
        const spec = await api.getPipeline(id)
        const { nodes: loadedNodes, edges: loadedEdges, skippedNodes } = fromSpec(spec)
        const loadedMeta = {
          id,
          name: spec.pipeline.name,
          description: spec.pipeline.description,
        }
        setNodes(loadedNodes)
        setEdges(loadedEdges)
        setName(spec.pipeline.name)
        setDescription(spec.pipeline.description)
        setCurrentId(id)
        setCurrentSource(pipelines?.find((p) => p.id === id)?.source ?? null)
        setSelectedNodeId(null)
        setBaseline(canvasFingerprint(loadedNodes, loadedEdges, loadedMeta))
        fitSoon()
        const stats = t('canvas.stats', { nodes: loadedNodes.length, edges: loadedEdges.length })
        if (skippedNodes > 0) {
          toast.success(t('load.successSkipped'), { id: toastId, description: stats })
        } else {
          toast.success(t('load.success'), { id: toastId, description: stats })
        }
      } catch (err) {
        toast.error(t('load.failed'), { id: toastId, description: errMsg(err) })
      }
    },
    [confirmDiscardIfDirty, pipelines, setNodes, setEdges, fitSoon, t],
  )

  /** 新建空白管线（file_input → file_output 最小模板） */
  const handleNewBlank = useCallback(async () => {
    if (!(await confirmDiscardIfDirty())) return
    const { nodes: n, edges: e } = blankTemplate()
    resetCanvas(n, e, t('canvas.untitledName'), '')
    fitSoon()
    toast.success(t('template.blankCreated'), { description: t('template.blankDescription') })
  }, [confirmDiscardIfDirty, resetCanvas, fitSoon, t])

  /** 载入本地示例模板（深拷贝，避免画布编辑污染模板） */
  const handleLoadExample = useCallback(async () => {
    if (!(await confirmDiscardIfDirty())) return
    // 每次调用时构建，确保模板节点标签取当前语言的文案
    const cloned = structuredClone(examplePipeline())
    resetCanvas(cloned.nodes, cloned.edges, t('canvas.exampleName'), '')
    fitSoon()
    toast.success(t('template.exampleLoaded'), { description: t('template.exampleDescription') })
  }, [confirmDiscardIfDirty, resetCanvas, fitSoon, t])

  /** 删除当前服务端管线（仅 custom；builtin 不显示删除按钮） */
  const handleDeletePipeline = useCallback(async () => {
    if (!currentId || currentSource !== 'custom') return
    const ok = await confirmDialog({
      title: t('deletePipeline.title'),
      description: t('deletePipeline.description', { name }),
      confirmLabel: t('common:action.delete'),
      cancelLabel: t('common:action.cancel'),
      variant: 'destructive',
    })
    if (!ok) return
    const toastId = toast.loading(t('deletePipeline.deleting'))
    try {
      await api.deletePipeline(currentId)
      toast.success(t('deletePipeline.success'), { id: toastId })
      resetCanvas([], [], t('canvas.untitledName'), '')
      refreshPipelines()
    } catch (err) {
      toast.error(t('deletePipeline.failed'), { id: toastId, description: errMsg(err) })
      refreshPipelines()
    }
  }, [currentId, currentSource, name, resetCanvas, refreshPipelines, t])

  // ---- 保存 ----

  /**
   * 保存到服务端。nameOverride 用于「另存为」对话框（此时 setName 尚未生效）。
   * 返回是否保存成功。
   */
  const savePipelineToServer = useCallback(
    async (id: string, opts?: { nameOverride?: string }): Promise<boolean> => {
      const effectiveName = opts?.nameOverride ?? name
      if (nodes.some((n) => n.data.kind === 'external')) {
        toast.error(t('save.cannotSave'), { description: t('save.externalNotSupported') })
        return false
      }
      if (nodes.length === 0) {
        toast.error(t('save.cannotSave'), { description: t('save.emptyCanvas') })
        return false
      }
      if (!effectiveName.trim()) {
        toast.error(t('save.cannotSave'), { description: t('save.nameEmpty') })
        return false
      }
      const badModule = nodes.find(
        (n) => n.data.kind === 'module' && (!n.data.moduleId || !n.data.capabilityId),
      )
      if (badModule) {
        toast.error(t('save.cannotSave'), {
          description: t('save.moduleIncomplete', { label: badModule.data.label }),
        })
        return false
      }
      const builtinTarget =
        currentSource === 'builtin' ||
        pipelines?.some((p) => p.id === id && p.source === 'builtin') === true
      if (builtinTarget) {
        const ok = await confirmDialog({
          title: t('save.overwriteTitle'),
          description: t('save.overwriteDescription'),
          confirmLabel: t('save.overwrite'),
          cancelLabel: t('common:action.cancel'),
          variant: 'destructive',
        })
        if (!ok) return false
      }
      const spec = toSpec(nodes, edges, { id, name: effectiveName, description })
      const toastId = toast.loading(t('save.saving'))
      try {
        await api.savePipeline(id, spec)
        setCurrentId(id)
        setCurrentSource(builtinTarget ? 'builtin' : 'custom')
        setBaseline(canvasFingerprint(nodes, edges, { id, name: effectiveName, description }))
        toast.success(t('save.success'), {
          id: toastId,
          description: builtinTarget
            ? t('save.statsOverwritten', { nodes: nodes.length, edges: edges.length })
            : t('canvas.stats', { nodes: nodes.length, edges: edges.length }),
        })
        refreshPipelines()
        return true
      } catch (err) {
        toast.error(t('save.failed'), { id: toastId, description: errMsg(err) })
        return false
      }
    },
    [nodes, edges, name, description, currentSource, pipelines, refreshPipelines, t],
  )

  /** 保存：已有服务端 id → 直接保存；否则打开「另存为」对话框 */
  const handleSave = useCallback(() => {
    if (currentId) {
      void savePipelineToServer(currentId)
    } else {
      setSaveAsOpen(true)
    }
  }, [currentId, savePipelineToServer])

  const handleSaveAsConfirm = useCallback(
    async (newName: string, newId: string): Promise<boolean> => {
      const ok = await savePipelineToServer(newId, { nameOverride: newName })
      if (ok) setName(newName)
      return ok
    },
    [savePipelineToServer],
  )

  /** 本地 JSON 定义文件导入（保留原有交互；导入后视为未保存的本地画布） */
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

      const loadedName = def.name
      setNodes(loadedNodes)
      setEdges(loadedEdges as Edge[])
      setName(loadedName)
      setDescription('')
      // 本地导入不绑定服务端管线
      setCurrentId(null)
      setCurrentSource(null)
      setSelectedNodeId(null)
      setBaseline(
        canvasFingerprint(loadedNodes, loadedEdges as Edge[], {
          id: '',
          name: loadedName,
          description: '',
        }),
      )
      requestAnimationFrame(() => {
        void fitView({ padding: 0.25, duration: 300 })
      })
      toast.success(t('load.success'), {
        description: t('load.nodeCount', { count: loadedNodes.length }),
      })
    },
    [setNodes, setEdges, fitView, t],
  )

  // ---- 校验与执行 ----

  const validatePipeline = useCallback((): string[] => {
    if (nodes.length === 0) return [t('validate.emptyCanvas')]
    const issues: string[] = []
    for (const n of nodes) {
      const { inputs, outputs } = getNodePorts(n.data)
      if (inputs.length > 0 && !edges.some((e) => e.target === n.id)) {
        issues.push(t('validate.missingInput', { label: n.data.label }))
      }
      if (outputs.length > 0 && !edges.some((e) => e.source === n.id)) {
        issues.push(t('validate.missingOutput', { label: n.data.label }))
      }
    }
    return issues
  }, [nodes, edges, t])

  const handleValidate = useCallback(() => {
    const issues = validatePipeline()
    if (issues.length === 0) {
      toast.success(t('validate.passed'), {
        description: t('validate.passedDescription', {
          nodes: nodes.length,
          edges: edges.length,
        }),
      })
    } else {
      toast.error(t('validate.issueCount', { count: issues.length }), {
        description:
          issues.slice(0, 4).join(t('validate.issueSeparator')) +
          (issues.length > 4 ? '…' : ''),
      })
    }
  }, [validatePipeline, nodes.length, edges.length, t])

  /**
   * 执行前置校验：连线完整性 + 必填参数校验（P1-21）+ file_input 存在性。
   * 空的必填参数不在此阻断，而是弹执行对话框补齐。
   */
  const handleExecute = useCallback(() => {
    if (executing) {
      toast.info(t('exec.alreadyRunning'), { description: t('exec.waitFinish') })
      return
    }
    const issues = validatePipeline()
    if (issues.length > 0) {
      toast.error(t('exec.validationFailed'), {
        description:
          issues.slice(0, 4).join(t('validate.issueSeparator')) +
          (issues.length > 4 ? '…' : ''),
      })
      return
    }
    if (nodes.some((n) => n.data.kind === 'external')) {
      toast.error(t('exec.cannotExecute'), { description: t('exec.externalNotSupported') })
      return
    }
    if (!nodes.some((n) => n.data.kind === 'builtin' && n.data.builtin === 'file_input')) {
      toast.error(t('exec.validationFailed'), { description: t('exec.fileInputRequired') })
      return
    }
    const missing = collectMissingRequired(nodes)
    if (missing.length > 0) {
      toast.info(t('exec.missingParamsTitle', { count: missing.length }), {
        description: t('exec.missingParamsHint'),
      })
    }
    setExecFields(buildExecFields(nodes))
    setExecDialogOpen(true)
  }, [executing, validatePipeline, nodes, t])

  /** 执行对话框提交：合并参数 → 清空旧状态 → executePipeline → 任务链接 */
  const handleSubmitExecution = useCallback(
    async (values: Record<string, ParamValue>) => {
      setExecSubmitting(true)
      try {
        // 1) 对话框值合并进节点 params（补齐的必填参数随画布保留）
        const patchByNode = new Map<string, NodeParams>()
        for (const f of execFields) {
          const v = values[f.key]
          if (v === undefined) continue
          const patch = patchByNode.get(f.nodeId) ?? {}
          patch[f.spec.name] = v
          patchByNode.set(f.nodeId, patch)
        }
        // 新一次执行开始：清空旧的节点运行状态
        const merged = nodes.map((n) => {
          const patch = patchByNode.get(n.id)
          return {
            ...n,
            data: {
              ...n.data,
              status: 'waiting',
              params: patch ? { ...n.data.params, ...patch } : n.data.params,
            },
          } as PipelineFlowNode
        })
        setNodes(merged)

        // 2) inputs：每个 file_input 节点注入本次执行的服务器文件路径
        const inputs: Record<string, Record<string, unknown>> = {}
        for (const n of merged) {
          if (n.data.kind === 'builtin' && n.data.builtin === 'file_input') {
            const path = n.data.params.path
            if (typeof path === 'string' && path.trim()) {
              inputs[n.id] = { path: path.trim() }
            }
          }
        }

        // 3) 画布 == 已加载的未修改服务端管线 → 按 id 执行；否则内联 spec
        const edited = execFields.some((f) => values[f.key] !== f.current)
        const sendById = currentId !== null && !dirty && !edited
        const body: ExecutePipelineRequest = sendById
          ? { pipeline_id: currentId, inputs }
          : {
              spec: toSpec(merged, edges, {
                id: currentId ?? fallbackPipelineId(name),
                name,
                description,
              }),
              inputs,
            }

        const resp = await api.executePipeline(body)
        executingRef.current = true
        setExecuting(true)
        setExecDialogOpen(false)
        toast.success(t('exec.submitted'), {
          description: t('exec.taskId', { taskId: resp.task_id }),
          duration: 8000,
          action: {
            label: (
              <Link to="/tasks" className="text-xs font-medium underline-offset-4 hover:underline">
                {t('exec.taskCenter')}
              </Link>
            ),
            onClick: () => {},
          },
        })
      } catch (err) {
        toast.error(t('exec.submitFailed'), { description: errMsg(err) })
      } finally {
        setExecSubmitting(false)
      }
    },
    [execFields, nodes, edges, currentId, dirty, name, description, setNodes, t],
  )

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

      <PipelineLibraryBar
        pipelines={pipelines}
        error={libraryError}
        onRefresh={refreshPipelines}
        currentId={currentId}
        currentSource={currentSource}
        dirty={dirty}
        executing={executing}
        onSelect={(id) => void handleSelectPipeline(id)}
        onNewBlank={() => void handleNewBlank()}
        onLoadExample={() => void handleLoadExample()}
        onSaveAs={() => setSaveAsOpen(true)}
        onDelete={() => void handleDeletePipeline()}
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
            onBeforeDelete={async ({ nodes: doomedNodes }) => {
              // 键盘 Delete/Backspace 及 React Flow 其他内置删除路径统一在此拦截确认；
              // 参数面板按钮删除走 requestDeleteNode，两者都接入 confirmDialog（P2-26）。
              if (doomedNodes.length === 0) return true
              const subject =
                doomedNodes.length === 1
                  ? t('node.deleteSubjectNode', { label: doomedNodes[0].data.label })
                  : t('node.deleteSubjectCount', { count: doomedNodes.length })
              return confirmDialog({
                title: t('node.deleteTitle'),
                description: t('node.deleteDescription', { subject }),
                confirmLabel: t('common:action.delete'),
                cancelLabel: t('common:action.cancel'),
                variant: 'destructive',
              })
            }}
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
                    {t(`nodeStatus.${key}`)}
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
              <p className="text-sm font-medium">{t('canvas.emptyTitle')}</p>
              <p className="text-xs text-muted-foreground">{t('canvas.emptyHint')}</p>
              <Button
                variant="outline"
                size="sm"
                className="pointer-events-auto"
                onClick={() => void handleLoadExample()}
              >
                <Sparkles className="h-3.5 w-3.5" />
                {t('canvas.loadExample')}
              </Button>
            </div>
          )}
        </div>

        {/* 桌面（≥lg）：参数面板常驻右栏；窄屏：右侧 overlay */}
        {selectedNode && (
          <NodeParamsPanel
            node={selectedNode}
            onParamsChange={(patch) => updateNodeParams(selectedNode.id, patch)}
            onDelete={() => void requestDeleteNode(selectedNode.id)}
            onClose={() => setSelectedNodeId(null)}
            className={
              isDesktop ? undefined : 'absolute inset-y-0 right-0 z-30 max-w-[85vw] shadow-lg'
            }
          />
        )}
      </div>

      <SaveAsDialog
        open={saveAsOpen}
        onOpenChange={setSaveAsOpen}
        defaultName={name}
        pipelines={pipelines}
        onConfirm={handleSaveAsConfirm}
      />

      <ExecuteDialog
        open={execDialogOpen}
        onOpenChange={setExecDialogOpen}
        fields={execFields}
        submitting={execSubmitting}
        onSubmit={(values) => void handleSubmitExecution(values)}
      />
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
