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
  ArrowRightLeft,
  ChevronDown,
  CircleCheck,
  Copy,
  Download,
  FolderOpen,
  History,
  Loader2,
  MemoryStick,
  Plus,
  RefreshCw,
  Sparkles,
  Trash2,
  TriangleAlert,
  Upload,
  Waypoints,
  X,
} from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type {
  CapabilityDecl,
  DeviceResponse,
  ExecutePipelineRequest,
  ModelListResponse,
  ModuleResponse,
  PipelineEdgeSpec,
  PipelineNodeSpec,
  PipelineSpec,
  PipelineSummary,
  TaskArtifact,
  TaskDetail,
  TaskSummary,
  VramBudgetResponse,
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
import { Switch } from '@/components/ui/switch'

import {
  BUILTIN_DEFS,
  DATA_TYPE_META,
  DRAG_MIME,
  NODE_STATUS_META,
  CapabilitySelect,
  ModuleBindingEditor,
  ParamSpecField,
  capabilitiesFromModule,
  capabilityFromDecl,
  createPipelineNode,
  dataTypesCompatible,
  defaultParams,
  getNodePorts,
  getParamSpecs,
  moduleNodeFieldsFromSpec,
  moduleNodeSpecFields,
  nodeKindLabel,
  normalizeNodeStatus,
  pipelineNodeTypes,
  serializeArgsParam,
} from '@/components/shared/pipeline-node'
import type {
  BuiltinFlowNode,
  BuiltinKind,
  CapabilityDef,
  DragPayload,
  ModuleFlowNode,
  ModuleNodeData,
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

// 深空仪表盘皮肤（W3 样式层；§6.2-E 保护条款：仅 --xy-* 换肤与连线外观）。
// 连线渐变引用画布容器内隐藏 SVG 的 #ep-edge-gradient（纯视觉标记）。
const RF_THEME_CSS = `
.react-flow {
  --xy-background-color: var(--card);
  --xy-background-pattern-color: var(--grid-dot);
  --xy-edge-stroke: color-mix(in srgb, var(--muted-foreground) 55%, transparent);
  --xy-edge-stroke-selected: var(--accent-gradient-from);
  --xy-edge-stroke-width: 2;
  --xy-connectionline-stroke: var(--accent-gradient-from);
  --xy-connectionline-stroke-width: 2;
  --xy-handle-background-color: var(--muted-foreground);
  --xy-handle-border-color: var(--card);
  --xy-controls-button-background-color: var(--surface-glass);
  --xy-controls-button-background-color-hover: var(--popover);
  --xy-controls-button-color: var(--muted-foreground);
  --xy-controls-button-color-hover: var(--foreground);
  --xy-controls-button-border-color: var(--border-glow);
  --xy-controls-box-shadow: none;
  --xy-minimap-background-color: var(--surface-glass);
  --xy-minimap-node-background-color: color-mix(in srgb, var(--muted-foreground) 75%, transparent);
  --xy-minimap-mask-background-color: color-mix(in srgb, var(--background) 55%, transparent);
  --xy-edge-label-background-color: var(--popover);
  --xy-edge-label-color: var(--muted-foreground);
  --xy-selection-background-color: color-mix(in srgb, var(--primary) 10%, transparent);
  --xy-selection-border: 1px dotted color-mix(in srgb, var(--primary) 70%, transparent);
  --xy-attribution-background-color: transparent;
}
.react-flow__controls {
  border: 1px solid var(--border-glow);
  border-radius: calc(var(--radius) - 2px);
  overflow: hidden;
  backdrop-filter: blur(8px);
}
.react-flow__controls-button {
  transition: background-color 150ms ease, color 150ms ease;
}
.react-flow__minimap {
  border: 1px solid var(--border-glow);
  border-radius: calc(var(--radius) - 2px);
  overflow: hidden;
}
.react-flow__attribution a {
  color: var(--muted-foreground);
}
.react-flow__edge-path {
  transition: stroke 150ms ease;
}
/* 选中边：品牌渐变描边（§3.1 规则 1 许可位「管线画布选中边」） */
.react-flow__edge.selected .react-flow__edge-path {
  stroke: url(#ep-edge-gradient) var(--accent-gradient-from);
}
/* 拖拽连线预览：渐变 + 流光 */
.react-flow__connection-path {
  stroke: url(#ep-edge-gradient) var(--accent-gradient-from);
  stroke-dasharray: 6 6;
  animation: ep-edge-flow 1.6s linear infinite;
}
/* 数据流态：执行中全部连线走针流光（1.6s linear 循环，§1 主张 6） */
.ep-executing .react-flow__edge-path {
  stroke-dasharray: 6 6;
  animation: ep-edge-flow 1.6s linear infinite;
}
@keyframes ep-edge-flow {
  to {
    stroke-dashoffset: -24;
  }
}
`

const DEFAULT_EDGE_OPTIONS = {
  type: 'default',
  markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16 },
  style: { strokeWidth: 2 },
}

// ============================================================
// 示例管线（本地模板：服务端列表为空时可一键载入，不再作为默认画布）
// ============================================================

/**
 * 示例模板能力兜底声明（daemon 不可达 / 模块列表未加载时降级用）。
 * 与 modules/ 下 manifest 的 input/output 类型一致，参数表降级为空。
 */
const EXAMPLE_DENOISE_DECL: CapabilityDecl = {
  name: 'denoise',
  description: '',
  input_type: 'audio',
  output_type: 'audio',
}
const EXAMPLE_TRANSCRIBE_DECL: CapabilityDecl = {
  name: 'transcribe',
  description: '',
  input_type: 'audio',
  output_type: 'json',
}

/** 数据驱动取模块能力（P0-1/P1-16）：模块列表优先，兜底最小声明 */
function exampleCapability(
  moduleList: ModuleResponse[] | null,
  moduleId: string,
  preferred: string,
  fallbackDecl: CapabilityDecl,
): { capabilities: CapabilityDef[]; capabilityId: string; capabilityLabel: string } {
  const decls = moduleList?.find((m) => m.id === moduleId)?.capabilities
  const caps = capabilitiesFromModule(decls)
  if (caps.length > 0) {
    const cap = caps.find((c) => c.id === preferred) ?? caps[0]!
    return { capabilities: caps, capabilityId: cap.id, capabilityLabel: cap.label }
  }
  const fb = capabilityFromDecl(fallbackDecl)
  return {
    capabilities: fb ? [fb] : [],
    capabilityId: fb?.id ?? preferred,
    capabilityLabel: fb?.label ?? preferred,
  }
}

function examplePipeline(
  moduleList: ModuleResponse[] | null,
): { nodes: PipelineFlowNode[]; edges: Edge[] } {
  // 修 P2-16：示例模板只引用仓库真实存在的模块
  // （modules/ 目录：deep-filter / faster-whisper / paddleocr / qwen3-tts / rembg，
  // 与 config/pipelines/*.toml 内置管线口径一致）；能力数据驱动（P0-1 收口）。
  const denoise = exampleCapability(moduleList, 'deep-filter', 'denoise', EXAMPLE_DENOISE_DECL)
  const asr = exampleCapability(moduleList, 'faster-whisper', 'transcribe', EXAMPLE_TRANSCRIBE_DECL)
  const denoiseCap = denoise.capabilities.find((c) => c.id === denoise.capabilityId)
  const asrCap = asr.capabilities.find((c) => c.id === asr.capabilityId)
  const denoiseModule = moduleList?.find((m) => m.id === 'deep-filter')
  const asrModule = moduleList?.find((m) => m.id === 'faster-whisper')
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
      id: 'demo-denoise',
      type: 'module',
      position: { x: 300, y: 50 },
      data: {
        kind: 'module',
        label: tp('template.exampleDenoiseNode', { defaultValue: 'DeepFilter 降噪' }),
        moduleId: 'deep-filter',
        moduleVersion: denoiseModule?.version ?? '0.5.6',
        category: denoiseModule?.category ?? 'denoise',
        capabilities: denoise.capabilities,
        capabilityId: denoise.capabilityId,
        capabilityLabel: denoise.capabilityLabel,
        status: 'waiting',
        params: defaultParams(denoiseCap?.params ?? []),
      },
    },
    {
      id: 'demo-asr',
      type: 'module',
      position: { x: 610, y: 210 },
      data: {
        kind: 'module',
        label: tp('template.exampleAsrNodeFw', { defaultValue: 'Faster-Whisper 语音识别' }),
        moduleId: 'faster-whisper',
        moduleVersion: asrModule?.version ?? '1.1.0',
        category: asrModule?.category ?? 'asr',
        capabilities: asr.capabilities,
        capabilityId: asr.capabilityId,
        capabilityLabel: asr.capabilityLabel,
        status: 'waiting',
        params: defaultParams(asrCap?.params ?? []),
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
          path: '/workspace/output/transcript.txt',
        },
      },
    },
  ]
  const edges: Edge[] = [
    {
      id: 'demo-e1',
      source: 'demo-input',
      target: 'demo-denoise',
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.file.label,
    },
    {
      id: 'demo-e2',
      source: 'demo-denoise',
      target: 'demo-asr',
      sourceHandle: 'out',
      targetHandle: 'in',
      label: DATA_TYPE_META.audio.label,
    },
    {
      id: 'demo-e3',
      source: 'demo-asr',
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
// §6.2 模块节点绑定字段（变体 pin / 设备绑定）——消费 C2 ModuleNodeData 字段
// ============================================================

/**
 * 模块节点绑定字段（C2 ModuleNodeData 已提供同名可选字段）：
 * - `model?: string`  — 变体 pin，空/缺省 = 跟随激活变体；
 * - `device?: string` — 设备绑定软约束 `"auto"` | `"cuda:0"` | …，缺省 = auto。
 */
interface ModuleBindingExt {
  model?: string
  device?: string
}

/** 读取模块节点的绑定字段（非 module 节点返回空） */
function getModuleBinding(data: PipelineNodeData): ModuleBindingExt {
  if (data.kind !== 'module') return {}
  return { model: data.model, device: data.device }
}

/**
 * 变体 pin 解析。两种合法形态（双形态兼容，仲裁请求见报告）：
 * - 完整形态 `<publisher>.<vendor>.<model>[@<variant>]`（§6.2 冻结示例）；
 * - 裸变体 `<variant>`（C2 ModuleBindingEditor 产出形态；与后端 vram.rs
 *   `rsplit('@')` 取变体的语义一致）。
 * 返回 null = 语法非法；variant 为 null = 仅 pin 模型族，变体跟随激活变体。
 */
const QUALIFIED_ID_RE = /^[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*){2}$/
const VARIANT_RE = /^[A-Za-z0-9.-]+$/

function parseModelPin(
  pin: string,
): { qualifiedId: string | null; variant: string | null } | null {
  const trimmed = pin.trim()
  if (!trimmed) return null
  const at = trimmed.indexOf('@')
  if (at < 0) {
    if (QUALIFIED_ID_RE.test(trimmed)) return { qualifiedId: trimmed, variant: null }
    if (VARIANT_RE.test(trimmed)) return { qualifiedId: null, variant: trimmed }
    return null
  }
  const qualifiedId = trimmed.slice(0, at)
  const variant = trimmed.slice(at + 1)
  if (!QUALIFIED_ID_RE.test(qualifiedId)) return null
  if (!variant || !VARIANT_RE.test(variant)) return null
  return { qualifiedId, variant }
}

/** 变体 pin 与激活变体的冲突条目（§5.2 MVP：报错 + 一键切换引导） */
interface PinIssue {
  /** mismatch = 与激活变体不一致；invalid = pin 语法非法；unknown_variant = 模块无此变体 */
  type: 'mismatch' | 'invalid' | 'unknown_variant'
  nodeId: string
  nodeLabel: string
  moduleId: string
  pin: string
  /** pin 的变体部分（mismatch / unknown_variant 时存在） */
  variant?: string
  /** 模块当前激活变体（mismatch 时存在） */
  active?: string
}

/**
 * 收集画布中所有模块节点的变体 pin 问题：
 * - pin 语法非法（§4.3 三段式 qualified_id 校验）；
 * - pin 的变体在模块变体列表中不存在（引导去统一页）;
 * - pin 的变体与激活变体不一致（active_models 无该模块条目时无法判定，跳过）。
 */
function collectPinIssues(
  nodes: PipelineFlowNode[],
  activeModels: Record<string, string> | null,
  modelsList: ModelListResponse | null,
): PinIssue[] {
  const issues: PinIssue[] = []
  for (const n of nodes) {
    if (n.data.kind !== 'module') continue
    const moduleId = n.data.moduleId
    const pin = getModuleBinding(n.data).model?.trim()
    if (!pin) continue
    const base: Pick<PinIssue, 'nodeId' | 'nodeLabel' | 'moduleId' | 'pin'> = {
      nodeId: n.id,
      nodeLabel: n.data.label,
      moduleId,
      pin,
    }
    const parsed = parseModelPin(pin)
    if (!parsed) {
      issues.push({ ...base, type: 'invalid' })
      continue
    }
    if (!parsed.variant) continue // 未 pin 变体 = 跟随激活变体
    const moduleModels = modelsList?.modules.find((m) => m.module_id === moduleId)?.models
    if (moduleModels && !moduleModels.some((m) => m.model_id === parsed.variant)) {
      issues.push({ ...base, type: 'unknown_variant', variant: parsed.variant })
      continue
    }
    const active = activeModels?.[moduleId]
    if (active !== undefined && active !== parsed.variant) {
      issues.push({ ...base, type: 'mismatch', variant: parsed.variant, active })
    }
  }
  return issues
}

/** MB → 人性化显示（≥1024 MB 转 GB） */
function formatMb(mb: number | null | undefined): string {
  if (mb === null || mb === undefined) return '—'
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`
  return `${mb} MB`
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

/**
 * P1-11 节点级超时/重试的前端挂载字段（后端 SpecNode 的 timeout_secs /
 * retry_count）。前端暂无编辑 UI，仅加载→保存往返保真（C2 节点数据类型
 * 不含这两个字段，页面层以交叉类型读写）。
 */
interface NodeTimingExt {
  timeoutSecs?: number
  retryCount?: number
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
    // P1-11 节点级超时/重试：前端无编辑面，加载时挂在 data 上（NodeTimingExt），
    // 保存时原样写回，保证 加载→保存 往返不丢失
    const timing = n.data as PipelineNodeData & NodeTimingExt
    const common = {
      id: n.id,
      label: n.data.label,
      params: { ...n.data.params },
      position: { x: n.position.x, y: n.position.y },
      ...(typeof timing.timeoutSecs === 'number' ? { timeout_secs: timing.timeoutSecs } : {}),
      ...(typeof timing.retryCount === 'number' ? { retry_count: timing.retryCount } : {}),
    }
    switch (n.data.kind) {
      case 'builtin': {
        const params = { ...n.data.params }
        // P0-2：ffmpeg args 序列化恒数组（遗留字符串形状按空白拆词归一，
        // 与后端 B7 防御性拆分的语义一致；C2 serializeArgsParam 统一出口）
        if (n.data.builtin === 'ffmpeg' && params.args !== undefined) {
          params.args = serializeArgsParam(params.args)
        }
        // §5.6：file_gate 的 media 条件在 UI 以平铺键（media_*）编辑，
        // 序列化时收敛为后端契约的嵌套 media 对象；仅写出已配置（>0）的条件
        if (n.data.builtin === 'file_gate') {
          const media: Record<string, number> = {}
          for (const [flat, nested] of FILE_GATE_MEDIA_KEYS) {
            const v = params[flat]
            if (typeof v === 'number' && Number.isFinite(v) && v > 0) media[nested] = v
          }
          for (const [flat] of FILE_GATE_MEDIA_KEYS) delete params[flat]
          if (Object.keys(media).length > 0) params.media = media as unknown as ParamValue
        }
        return { ...common, params, kind: 'builtin', builtin: n.data.builtin }
      }
      case 'module':
        // §6.2 契约字段（capability 裸名 + model/device；C2 moduleNodeSpecFields
        // 负责「空串/auto 等价缺省不输出」语义，仲裁 #2/#28）
        return {
          ...common,
          kind: 'module',
          module_id: n.data.moduleId,
          ...moduleNodeSpecFields(n.data),
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

/**
 * VRAM 预算专用 spec 投影（§6.3）：只保留后端 vram-budget 消费的字段
 * （id/kind/module_id/capability/model/device + 边），跳过 external 节点。
 * 画布为空（无可用节点）返回 null（后端对空 spec 返回 400，前端先行拦截）。
 */
function buildBudgetSpec(
  nodes: PipelineFlowNode[],
  edges: Edge[],
): PipelineSpec | null {
  const usable = nodes.filter(
    (n): n is ModuleFlowNode | BuiltinFlowNode => n.data.kind !== 'external',
  )
  if (usable.length === 0) return null
  const ids = new Set(usable.map((n) => n.id))
  const specNodes: PipelineNodeSpec[] = usable.map((n) => {
    if (n.data.kind === 'builtin') {
      return {
        id: n.id,
        label: n.data.label,
        kind: 'builtin' as const,
        builtin: n.data.builtin,
        params: {},
      }
    }
    return {
      id: n.id,
      label: n.data.label,
      kind: 'module' as const,
      module_id: n.data.moduleId,
      params: {},
      ...moduleNodeSpecFields(n.data),
    }
  })
  const specEdges: PipelineEdgeSpec[] = edges
    .filter((e) => ids.has(e.source) && ids.has(e.target))
    .map((e) => ({
      from: [e.source, e.sourceHandle ?? 'out'] as [string, string],
      to: [e.target, e.targetHandle ?? 'in'] as [string, string],
    }))
  return {
    pipeline: { id: 'vram-budget', name: 'vram-budget', description: '' },
    nodes: specNodes,
    edges: specEdges,
  }
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
 * file_gate media 条件的 UI 平铺键 ↔ 后端契约嵌套键（§5.6）。
 * UI 侧 `media_*` 平铺编辑，spec 侧收敛为嵌套 `media` 对象。
 */
const FILE_GATE_MEDIA_KEYS: [flat: string, nested: string][] = [
  ['media_min_duration_secs', 'min_duration_secs'],
  ['media_max_duration_secs', 'max_duration_secs'],
  ['media_min_width', 'min_width'],
  ['media_min_height', 'min_height'],
]

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
 * - module 节点：能力/分类/版本按 moduleList（api.modules()）数据驱动恢复
 *   （P0-1 收口，capability 为裸名契约）；模块未安装时降级为无能力空态；
 * - §6.2 model/device 与 P1-11 timeout/retry 原样恢复（往返不丢失）；
 * - params 原样保留（含 UI 不展示字段，往返不丢失）；
 * - position 缺失的节点走级联布局；
 * - 引用未知节点的边被过滤。
 */
function fromSpec(
  spec: PipelineSpec,
  moduleList: ModuleResponse[] | null,
): {
  nodes: PipelineFlowNode[]
  edges: Edge[]
  skippedNodes: number
} {
  const layout = cascadeLayout(spec)
  const nodes: PipelineFlowNode[] = []
  const known = new Set<string>()
  let skippedNodes = 0

  /** P1-11 超时/重试挂载（前端无编辑面，仅往返保真） */
  const applyTiming = (data: PipelineNodeData, sn: PipelineNodeSpec) => {
    const ext = data as PipelineNodeData & NodeTimingExt
    if (typeof sn.timeout_secs === 'number') ext.timeoutSecs = sn.timeout_secs
    if (typeof sn.retry_count === 'number') ext.retryCount = sn.retry_count
  }

  for (const sn of spec.nodes ?? []) {
    const position = sn.position ?? layout.get(sn.id) ?? { x: 0, y: 0 }
    // 保留非 UI 展示的参数值：运行时原样存取，toSpec 时整体写回
    const params = (sn.params ?? {}) as NodeParams

    if (sn.kind === 'builtin') {
      const builtin = sn.builtin as BuiltinKind | undefined
      if (builtin && builtin in BUILTIN_DEFS) {
        // §5.6：file_gate 的嵌套 media 对象还原为 UI 平铺键（往返不丢失；
        // toSpec 序列化时按原键收敛回嵌套对象）
        if (builtin === 'file_gate' && params.media && typeof params.media === 'object' && !Array.isArray(params.media)) {
          const media = params.media as Record<string, unknown>
          for (const [flat, nested] of FILE_GATE_MEDIA_KEYS) {
            const v = media[nested]
            if (typeof v === 'number' && Number.isFinite(v)) params[flat] = v
          }
          delete params.media
        }
        const data: PipelineNodeData = {
          kind: 'builtin',
          builtin,
          label: sn.label || BUILTIN_DEFS[builtin].label,
          status: 'waiting',
          params,
        }
        applyTiming(data, sn)
        nodes.push({ id: sn.id, type: 'builtin', position, data })
        known.add(sn.id)
        continue
      }
      skippedNodes += 1
      continue
    }

    if (sn.kind === 'module') {
      // 能力数据驱动：按 module_id 查已安装模块的 manifest capabilities
      const moduleInfo = moduleList?.find((m) => m.id === sn.module_id) ?? null
      const caps = capabilitiesFromModule(moduleInfo?.capabilities)
      const fields = moduleNodeFieldsFromSpec(sn)
      const cap = caps.find((c) => c.id === fields.capabilityId) ?? null
      const data: ModuleNodeData = {
        kind: 'module',
        label: sn.label || cap?.label || fields.capabilityId || sn.module_id || 'unknown',
        moduleId: sn.module_id || 'unknown',
        // spec 契约不含版本：取已安装模块版本，未知时占位展示
        moduleVersion: moduleInfo?.version ?? '1.0.0',
        category: moduleInfo?.category ?? 'other',
        capabilities: caps,
        capabilityId: fields.capabilityId || cap?.id || '',
        capabilityLabel: cap?.label ?? fields.capabilityId,
        status: 'waiting',
        params,
      }
      // §6.2 变体 pin / 设备绑定（C2 ModuleNodeData 字段）
      if (fields.model) data.model = fields.model
      if (fields.device) data.device = fields.device
      applyTiming(data, sn)
      nodes.push({ id: sn.id, type: 'module', position, data })
      known.add(sn.id)
      continue
    }

    // 未知 kind：前端 spec 契约仅 builtin/module（external_api 已由后端 §6.7
    // 归一为 builtin llm），不认识的种类跳过并计数，加载 toast 中提示
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
 * 画布指纹（仅覆盖 toSpec 实际输出的契约字段）：用于判断相对上次加载 / 保存
 * 是否存在未保存更改。capabilities/category/版本等水合元数据不进指纹，
 * 模块列表异步到达后的能力水合不会误报「未保存更改」。
 */
function canvasFingerprint(
  nodes: PipelineFlowNode[],
  edges: Edge[],
  meta: CanvasMeta,
): string {
  return canonicalJson({
    meta,
    nodes: nodes.map((n) => {
      const base = {
        id: n.id,
        position: n.position,
        label: n.data.label,
        params: n.data.params,
      }
      switch (n.data.kind) {
        case 'builtin':
          return { ...base, kind: 'builtin', builtin: n.data.builtin }
        case 'module':
          return {
            ...base,
            kind: 'module',
            module_id: n.data.moduleId,
            ...moduleNodeSpecFields(n.data),
          }
        case 'external':
          return { ...base, kind: 'external', endpoint: n.data.endpoint, method: n.data.method }
      }
    }),
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
// 管线 TOML 导出 / 导入（§6.4）
// ============================================================

/**
 * TOML 基本字符串：JSON.stringify 的转义序列（\" \\ \n \t \uXXXX…）
 * 与 TOML 基本字符串兼容，直接复用。
 */
function tomlString(s: string): string {
  return JSON.stringify(s)
}

/** TOML 键：裸键仅 [A-Za-z0-9_-]，否则加引号（与后端 bridge toml_key 同款策略） */
function tomlKey(k: string): string {
  return /^[A-Za-z0-9_-]+$/.test(k) ? k : tomlString(k)
}

/**
 * JSON 值 → TOML 值文本；无法表达（null/undefined/非有限数）返回 null。
 * 对象 → 行内表（与后端 bridge 输出口径一致）。
 */
function tomlValue(v: unknown): string | null {
  if (typeof v === 'string') return tomlString(v)
  if (typeof v === 'number') {
    if (!Number.isFinite(v)) return null
    return String(v)
  }
  if (typeof v === 'boolean') return v ? 'true' : 'false'
  if (Array.isArray(v)) {
    const parts: string[] = []
    for (const item of v) {
      const s = tomlValue(item)
      if (s === null) return null
      parts.push(s)
    }
    return `[${parts.join(', ')}]`
  }
  if (v && typeof v === 'object') {
    const parts: string[] = []
    for (const [k, val] of Object.entries(v as Record<string, unknown>)) {
      if (val === undefined || val === null) continue // TOML 无 null
      const s = tomlValue(val)
      if (s === null) return null
      parts.push(`${tomlKey(k)} = ${s}`)
    }
    return `{ ${parts.join(', ')} }`
  }
  return null
}

/** 前端 handle id（in/out）→ ep-core 端口名（input/output）；其余原样 */
function exportPortName(handle: string | null | undefined, fallback: 'input' | 'output'): string {
  if (!handle) return fallback
  if (handle === 'in') return 'input'
  if (handle === 'out') return 'output'
  return handle
}

/** 导出依赖清单行：显式 pin 的 `<qualified_id>@<variant>`（§6.4 头部注释） */
function collectDependencyLines(nodes: PipelineFlowNode[]): string[] {
  const lines: string[] = []
  for (const n of nodes) {
    if (n.data.kind !== 'module') continue
    const pin = getModuleBinding(n.data).model?.trim()
    if (pin) lines.push(`${pin}（节点 ${n.id} · 模块 ${n.data.moduleId}）`)
  }
  return lines
}

/**
 * 管线 spec → TOML 文本（§6.4 导出）：头部注释依赖清单 +
 * `[pipeline]` / `[[nodes]]` / `[[edges]]` 标准布局（与后端 bridge 输出口径一致，
 * 可被 ep_core::load_pipeline 直接读回）。
 */
function specToToml(spec: PipelineSpec, dependencyLines: string[]): string {
  const out: string[] = []
  out.push('# ── EntryPoint 管线编辑器导出 ──')
  out.push(`# 导出时间: ${new Date().toISOString()}`)
  out.push('# 依赖清单（模型 / 变体 pin）:')
  if (dependencyLines.length === 0) {
    out.push('#   - （无显式 pin；模块节点均跟随激活变体）')
  } else {
    for (const d of dependencyLines) out.push(`#   - ${d}`)
  }
  out.push('')

  out.push('[pipeline]')
  out.push(`id = ${tomlString(spec.pipeline.id)}`)
  out.push(`name = ${tomlString(spec.pipeline.name)}`)
  out.push(`description = ${tomlString(spec.pipeline.description)}`)

  for (const node of spec.nodes ?? []) {
    out.push('')
    out.push('[[nodes]]')
    out.push(`id = ${tomlString(node.id)}`)
    if (node.kind === 'builtin') {
      out.push('kind = "builtin"')
      out.push(`builtin = ${tomlString(node.builtin ?? '')}`)
    } else {
      out.push('kind = "module"')
      out.push(`module_id = ${tomlString(node.module_id ?? '')}`)
      out.push(`capability = ${tomlString(node.capability ?? '')}`)
      if (node.model) out.push(`model = ${tomlString(node.model)}`)
      if (node.device) out.push(`device = ${tomlString(node.device)}`)
    }
    if (node.label) out.push(`label = ${tomlString(node.label)}`)
    if (node.position) {
      out.push(`position = { x = ${node.position.x}, y = ${node.position.y} }`)
    }
    const params = tomlValue(node.params ?? {})
    if (params && params !== '{  }') out.push(`params = ${params}`)
  }

  for (const edge of spec.edges ?? []) {
    out.push('')
    out.push('[[edges]]')
    out.push(
      `from = [${tomlString(edge.from[0])}, ${tomlString(exportPortName(edge.from[1], 'output'))}]`,
    )
    out.push(
      `to = [${tomlString(edge.to[0])}, ${tomlString(exportPortName(edge.to[1], 'input'))}]`,
    )
  }
  out.push('')
  return out.join('\n')
}

// ---- 最小 TOML 解析器（管线文件子集，导入校验用） ----

type TomlValue = string | number | boolean | TomlValue[] | { [key: string]: TomlValue }

function tomlParseError(line: number, message: string): Error {
  return new Error(`${message}（第 ${line} 行）`)
}

/** 去掉行尾注释（识别基本字符串/字面量字符串内的 # 不当作注释） */
function stripTomlComment(line: string): string {
  let out = ''
  let inBasic = false
  let inLiteral = false
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i]
    if (inBasic) {
      out += c
      if (c === '\\') {
        out += line[i + 1] ?? ''
        i += 1
      } else if (c === '"') {
        inBasic = false
      }
      continue
    }
    if (inLiteral) {
      out += c
      if (c === "'") inLiteral = false
      continue
    }
    if (c === '"') {
      inBasic = true
      out += c
      continue
    }
    if (c === "'") {
      inLiteral = true
      out += c
      continue
    }
    if (c === '#') break
    out += c
  }
  return out
}

/** 统计一行中字符串外的括号深度增量（[ { 加，] } 减） */
function tomlBracketDelta(line: string): number {
  let depth = 0
  let inBasic = false
  let inLiteral = false
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i]
    if (inBasic) {
      if (c === '\\') i += 1
      else if (c === '"') inBasic = false
      continue
    }
    if (inLiteral) {
      if (c === "'") inLiteral = false
      continue
    }
    if (c === '"') inBasic = true
    else if (c === "'") inLiteral = true
    else if (c === '[' || c === '{') depth += 1
    else if (c === ']' || c === '}') depth -= 1
  }
  return depth
}

function skipTomlWs(s: string, i: number): number {
  while (i < s.length && (s[i] === ' ' || s[i] === '\t')) i += 1
  return i
}

/** 解析 TOML 基本字符串（含转义），返回 [值, 结束下标] */
function parseTomlBasicString(s: string, start: number, line: number): [string, number] {
  let out = ''
  let i = start + 1
  while (i < s.length) {
    const c = s[i]
    if (c === '"') return [out, i + 1]
    if (c === '\\') {
      const next = s[i + 1]
      let consumed = 2
      if (next === '"' || next === '\\') out += next
      else if (next === 'n') out += '\n'
      else if (next === 't') out += '\t'
      else if (next === 'r') out += '\r'
      else if (next === 'b') out += '\b'
      else if (next === 'f') out += '\f'
      else if (next === 'u' || next === 'U') {
        const len = next === 'u' ? 4 : 8
        const hex = s.slice(i + 2, i + 2 + len)
        if (!/^[0-9a-fA-F]+$/.test(hex)) throw tomlParseError(line, '非法的 \\u 转义')
        out += String.fromCodePoint(Number.parseInt(hex, 16))
        consumed = 2 + len
      } else {
        throw tomlParseError(line, `不支持的转义序列 \\${next ?? ''}`)
      }
      i += consumed
      continue
    }
    out += c
    i += 1
  }
  throw tomlParseError(line, '字符串未闭合')
}

/** 解析单个 TOML 值（字符串/数字/布尔/数组/行内表），返回 [值, 结束下标] */
function parseTomlValue(s: string, start: number, line: number): [TomlValue, number] {
  const i = skipTomlWs(s, start)
  const c = s[i]
  if (c === undefined) throw tomlParseError(line, '缺少值')
  if (c === '"') {
    if (s.startsWith('"""', i)) throw tomlParseError(line, '不支持多行字符串')
    return parseTomlBasicString(s, i, line)
  }
  if (c === "'") {
    if (s.startsWith("'''", i)) throw tomlParseError(line, '不支持多行字符串')
    const end = s.indexOf("'", i + 1)
    if (end < 0) throw tomlParseError(line, '字面量字符串未闭合')
    return [s.slice(i + 1, end), end + 1]
  }
  if (c === '[') {
    const arr: TomlValue[] = []
    let j = i + 1
    for (;;) {
      j = skipTomlWs(s, j)
      if (s[j] === ']') return [arr, j + 1]
      const [item, next] = parseTomlValue(s, j, line)
      arr.push(item)
      j = skipTomlWs(s, next)
      if (s[j] === ',') {
        j += 1
        continue
      }
      if (s[j] === ']') return [arr, j + 1]
      throw tomlParseError(line, '数组元素间缺少逗号或 ]')
    }
  }
  if (c === '{') {
    const obj: { [key: string]: TomlValue } = {}
    let j = i + 1
    for (;;) {
      j = skipTomlWs(s, j)
      if (s[j] === '}') return [obj, j + 1]
      let key: string
      if (s[j] === '"') {
        const [k, next] = parseTomlBasicString(s, j, line)
        key = k
        j = next
      } else if (s[j] === "'") {
        const end = s.indexOf("'", j + 1)
        if (end < 0) throw tomlParseError(line, '键字符串未闭合')
        key = s.slice(j + 1, end)
        j = end + 1
      } else {
        const m = /^[A-Za-z0-9_-]+/.exec(s.slice(j))
        if (!m) throw tomlParseError(line, '行内表缺少键')
        key = m[0]
        j += m[0].length
      }
      j = skipTomlWs(s, j)
      if (s[j] !== '=') throw tomlParseError(line, '行内表键后缺少 =')
      const [value, next] = parseTomlValue(s, j + 1, line)
      obj[key] = value
      j = skipTomlWs(s, next)
      if (s[j] === ',') {
        j += 1
        continue
      }
      if (s[j] === '}') return [obj, j + 1]
      throw tomlParseError(line, '行内表项间缺少逗号或 }')
    }
  }
  if (s.startsWith('true', i)) return [true, i + 4]
  if (s.startsWith('false', i)) return [false, i + 5]
  const m = /^[+-]?(?:inf|nan|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)/.exec(s.slice(i))
  if (m) {
    const raw = m[0].replace(/_/g, '')
    const value =
      raw === 'inf' || raw === '+inf'
        ? Number.POSITIVE_INFINITY
        : raw === '-inf'
          ? Number.NEGATIVE_INFINITY
          : raw === 'nan' || raw === '+nan' || raw === '-nan'
            ? Number.NaN
            : raw.includes('.') || raw.includes('e') || raw.includes('E')
              ? Number.parseFloat(raw)
              : Number.parseInt(raw, 10)
    if (Number.isNaN(value) && !raw.includes('nan')) {
      throw tomlParseError(line, `非法数字: ${m[0]}`)
    }
    return [value, i + m[0].length]
  }
  throw tomlParseError(line, `无法解析的值: ${s.slice(i, i + 20)}`)
}

/** 段头名归一（容忍引号与空格） */
function tomlSectionName(raw: string): string {
  return raw
    .split('.')
    .map((seg) => seg.trim().replace(/^"(.*)"$/, '$1').replace(/^'(.*)'$/, '$1'))
    .join('.')
}

/**
 * 最小 TOML 解析器（管线文件子集，§6.4 导入校验）：
 * 支持 `[pipeline]` / `[[nodes]]` / `[nodes.params]` / `[[edges]]` 段与
 * 基本/字面量字符串、数字、布尔、数组、行内表值（后端 bridge 导出的全部形态）。
 * 不支持的语法（多行字符串、日期等）抛带行号的错误。
 */
function parsePipelineToml(text: string): PipelineSpec {
  const rawLines = text.split(/\r?\n/)

  // 阶段 1：去注释 + 跨行括号合并为逻辑行
  const logical: { text: string; line: number }[] = []
  let buffer = ''
  let bufferLine = 0
  let depth = 0
  for (let idx = 0; idx < rawLines.length; idx += 1) {
    const stripped = stripTomlComment(rawLines[idx]).trim()
    if (buffer) {
      buffer += ` ${stripped}`
      depth += tomlBracketDelta(stripped)
      if (depth <= 0) {
        logical.push({ text: buffer, line: bufferLine })
        buffer = ''
        depth = 0
      }
      continue
    }
    if (!stripped) continue
    const delta = tomlBracketDelta(stripped)
    if (delta > 0 && !stripped.startsWith('[[') && !stripped.startsWith('[')) {
      // key = [ ... 未闭合（段头 [xxx] 自身平衡，不会进此分支）
      buffer = stripped
      bufferLine = idx + 1
      depth = delta
      continue
    }
    logical.push({ text: stripped, line: idx + 1 })
  }
  if (buffer) throw tomlParseError(bufferLine, '数组或行内表未闭合')

  // 阶段 2：逐语句解析到原始结构
  let pipelineMeta: { [key: string]: TomlValue } = {}
  const rawNodes: { [key: string]: TomlValue }[] = []
  const rawEdges: { [key: string]: TomlValue }[] = []
  let section = 'other'

  for (const { text, line } of logical) {
    if (text.startsWith('[[')) {
      const end = text.indexOf(']]')
      if (end < 0) throw tomlParseError(line, '段头未闭合')
      const name = tomlSectionName(text.slice(2, end))
      if (name === 'nodes') {
        rawNodes.push({})
        section = 'nodes'
      } else if (name === 'edges') {
        rawEdges.push({})
        section = 'edges'
      } else {
        section = 'other'
      }
      continue
    }
    if (text.startsWith('[')) {
      const end = text.indexOf(']')
      if (end < 0) throw tomlParseError(line, '段头未闭合')
      const name = tomlSectionName(text.slice(1, end))
      if (name === 'pipeline') section = 'pipeline'
      else if (name === 'nodes.params') section = 'nodes.params'
      else section = 'other'
      continue
    }

    const keyMatch = /^("(?:[^"\\]|\\.)*"|'[^']*'|[A-Za-z0-9_-]+)\s*=/.exec(text)
    if (!keyMatch) throw tomlParseError(line, `无法解析的语句: ${text.slice(0, 24)}`)
    const key = keyMatch[1].replace(/^"(.*)"$/, '$1').replace(/^'(.*)'$/, '$1')
    const [value] = parseTomlValue(text, keyMatch[0].length, line)

    if (section === 'pipeline') {
      pipelineMeta[key] = value
    } else if (section === 'nodes') {
      if (rawNodes.length === 0) throw tomlParseError(line, '[[nodes]] 段缺失')
      rawNodes[rawNodes.length - 1][key] = value
    } else if (section === 'nodes.params') {
      if (rawNodes.length === 0) throw tomlParseError(line, '[[nodes]] 段缺失')
      const node = rawNodes[rawNodes.length - 1]
      const params = (node.params as { [key: string]: TomlValue } | undefined) ?? {}
      params[key] = value
      node.params = params
    } else if (section === 'edges') {
      if (rawEdges.length === 0) throw tomlParseError(line, '[[edges]] 段缺失')
      rawEdges[rawEdges.length - 1][key] = value
    }
    // other 段：忽略（容忍未知扩展段）
  }

  // 阶段 3：组装 PipelineSpec
  const id = typeof pipelineMeta.id === 'string' ? pipelineMeta.id : ''
  if (!id) throw new Error('缺少 [pipeline] id')
  const specNodes: PipelineNodeSpec[] = rawNodes.map((raw, index) => {
    const nodeId = typeof raw.id === 'string' ? raw.id : ''
    if (!nodeId) throw new Error(`第 ${index + 1} 个 [[nodes]] 缺少 id`)
    const kind = typeof raw.kind === 'string' ? raw.kind : ''
    if (kind !== 'builtin' && kind !== 'module') {
      throw new Error(`节点 ${nodeId} 的 kind 非法或缺失: ${kind || '(无)'}`)
    }
    const str = (v: TomlValue | undefined): string | undefined =>
      typeof v === 'string' && v ? v : undefined
    const node: PipelineNodeSpec = {
      id: nodeId,
      label: str(raw.label) ?? '',
      kind,
      params: (raw.params as Record<string, unknown> | undefined) ?? {},
    }
    if (kind === 'builtin') {
      node.builtin = str(raw.builtin) ?? ''
    } else {
      node.module_id = str(raw.module_id) ?? ''
      node.capability = str(raw.capability) ?? ''
    }
    if (str(raw.model)) node.model = str(raw.model)
    if (str(raw.device)) node.device = str(raw.device)
    if (raw.position && typeof raw.position === 'object' && !Array.isArray(raw.position)) {
      const p = raw.position as { [key: string]: TomlValue }
      if (typeof p.x === 'number' && typeof p.y === 'number') {
        node.position = { x: p.x, y: p.y }
      }
    }
    return node
  })

  const specEdges: PipelineEdgeSpec[] = rawEdges.map((raw, index) => {
    const side = (v: TomlValue | undefined, label: string): [string, string] => {
      if (
        !Array.isArray(v) ||
        v.length !== 2 ||
        typeof v[0] !== 'string' ||
        typeof v[1] !== 'string'
      ) {
        throw new Error(`第 ${index + 1} 条 [[edges]] 的 ${label} 必须是 [节点, 端口]`)
      }
      return [v[0], v[1]]
    }
    return { from: side(raw.from, 'from'), to: side(raw.to, 'to') }
  })

  return {
    pipeline: {
      id,
      name: typeof pipelineMeta.name === 'string' && pipelineMeta.name ? pipelineMeta.name : id,
      description: typeof pipelineMeta.description === 'string' ? pipelineMeta.description : '',
    },
    nodes: specNodes,
    edges: specEdges,
  }
}

/** 导入依赖检查提示条目（缺模块 / 缺变体 / 无法渲染的 builtin） */
interface ImportIssue {
  level: 'warn' | 'info'
  text: string
}

/**
 * 导入 TOML → 依赖解析提示（§6.4：缺模型/变体列表引导去统一页）。
 * 仅提示不阻断注册；模块/模型列表未加载完成时跳过对应检查。
 */
function collectImportIssues(
  spec: PipelineSpec,
  moduleList: ModuleResponse[] | null,
  modelsList: ModelListResponse | null,
  t: (key: string, options?: Record<string, unknown>) => string,
): ImportIssue[] {
  const issues: ImportIssue[] = []
  for (const node of spec.nodes) {
    if (node.kind === 'builtin') {
      if (node.builtin && !(node.builtin in BUILTIN_DEFS)) {
        issues.push({
          level: 'info',
          text: t('io.builtinSkipped', {
            defaultValue: '内置节点类型 {{builtin}}（节点 {{id}}）暂无法在画布渲染，加载时将跳过',
            builtin: node.builtin,
            id: node.id,
          }),
        })
      }
      continue
    }
    const moduleId = node.module_id ?? ''
    if (moduleList && moduleId && !moduleList.some((m) => m.id === moduleId)) {
      issues.push({
        level: 'warn',
        text: t('io.missingModule', {
          defaultValue: '缺少模块 {{moduleId}}（节点 {{id}}）— 本机未安装该模块',
          moduleId,
          id: node.id,
        }),
      })
      continue
    }
    const pin = node.model?.trim()
    if (!pin) continue
    const parsed = parseModelPin(pin)
    if (!parsed) {
      issues.push({
        level: 'warn',
        text: t('io.invalidPin', {
          defaultValue: '节点 {{id}} 的变体 pin 语法非法: {{pin}}',
          id: node.id,
          pin,
        }),
      })
      continue
    }
    if (!parsed.variant || !modelsList) continue
    const variants = modelsList.modules.find((m) => m.module_id === moduleId)?.models
    if (variants && !variants.some((m) => m.model_id === parsed.variant)) {
      issues.push({
        level: 'warn',
        text: t('io.missingVariant', {
          defaultValue: '模块 {{moduleId}} 缺少变体 {{variant}}（节点 {{id}}）— 请到统一页下载或切换',
          moduleId,
          variant: parsed.variant,
          id: node.id,
        }),
      })
    }
  }
  return issues
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

/**
 * 空值定义：未设置 / 空字符串（含纯空白）/ 全空条目的数组（ffmpeg args，P0-2）。
 * 数字 0、布尔 false 不算空。
 */
function isParamEmpty(value: ParamValue | undefined): boolean {
  if (value === undefined) return true
  if (typeof value === 'string') return value.trim() === ''
  if (Array.isArray(value)) return value.every((item) => item.trim() === '')
  return false
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
// 右侧参数面板（参数字段渲染整体委托 C2 ParamSpecField：
// 覆盖 textarea / string_array 等全类型，修 llm.system_prompt 与 ffmpeg.args 不渲染）
// ============================================================

interface NodeParamsPanelProps {
  node: PipelineFlowNode
  onParamsChange: (patch: NodeParams) => void
  onDelete: () => void
  onClose: () => void
  /** 附加布局类（窄屏 overlay 定位等） */
  className?: string
  /** module 节点：绑定字段（§6.2 model/device）更新 */
  onBindingChange?: (patch: ModuleBindingExt) => void
  /** module 节点：能力切换（调用方按新能力重建默认参数） */
  onCapabilityChange?: (capabilityId: string) => void
  /** module 节点：该模块的变体列表（model_id，models API） */
  variants?: string[]
  /** 本机设备列表（/api/devices；device 软约束下拉选项） */
  devices?: DeviceResponse[]
  /** 该模块当前激活变体（config.active_models；undefined = 未知/跟随默认变体） */
  activeVariant?: string
}

function NodeParamsPanel({
  node,
  onParamsChange,
  onDelete,
  onClose,
  className,
  onBindingChange,
  onCapabilityChange,
  variants,
  devices,
  activeVariant,
}: NodeParamsPanelProps) {
  const { t } = useTranslation('pipeline')
  const specs = getParamSpecs(node.data)
  const status = NODE_STATUS_META[node.data.status]
  const moduleData = node.data.kind === 'module' ? node.data : null
  const binding = getModuleBinding(node.data)

  // 变体 pin 即时提示：与激活变体不一致（执行前将阻断并给出切换引导，§5.2）
  const pinnedVariant = binding.model?.trim() ? parseModelPin(binding.model)?.variant : null
  const pinMismatch =
    pinnedVariant && activeVariant !== undefined && activeVariant !== pinnedVariant
  const pinUnknownVariant =
    pinnedVariant && (variants?.length ?? 0) > 0 && !variants?.includes(pinnedVariant)

  return (
    <aside
      className={cn(
        'glass flex h-full w-72 shrink-0 flex-col border-l border-border-glow',
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

          {moduleData && (
            <section className="space-y-3">
              <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                {t('nodePanel.bindingTitle', { defaultValue: '能力与绑定' })}
              </h3>
              <CapabilitySelect
                data={moduleData}
                onChange={(capabilityId) => onCapabilityChange?.(capabilityId)}
              />
              <ModuleBindingEditor
                model={binding.model}
                device={binding.device}
                variants={variants ?? []}
                devices={devices ?? []}
                onChange={(patch) => onBindingChange?.(patch)}
              />
              <p className="text-[10px] text-muted-foreground">
                {activeVariant !== undefined
                  ? t('nodePanel.activeVariant', {
                      defaultValue: '当前激活变体：{{variant}}',
                      variant: activeVariant,
                    })
                  : t('nodePanel.activeVariantDefault', {
                      defaultValue: '未显式设置激活变体（跟随 manifest 默认变体）',
                    })}
              </p>
              {pinMismatch && (
                <p className="flex items-start gap-1.5 rounded-md border border-status-starting/30 bg-status-starting/10 px-2.5 py-2 text-[11px] text-status-starting">
                  <TriangleAlert className="mt-px h-3 w-3 shrink-0" />
                  {t('nodePanel.pinMismatchHint', {
                    defaultValue: 'pin 与激活变体不一致：执行前将阻断，并引导一键切换激活变体（§5.2）',
                  })}
                </p>
              )}
              {pinUnknownVariant && (
                <p className="flex items-start gap-1.5 rounded-md border border-status-starting/30 bg-status-starting/10 px-2.5 py-2 text-[11px] text-status-starting">
                  <TriangleAlert className="mt-px h-3 w-3 shrink-0" />
                  {t('nodePanel.pinUnknownVariantHint', {
                    defaultValue: '该模块没有此变体，请到模型统一页核对（执行前将阻断）',
                  })}
                </p>
              )}
            </section>
          )}

          <section className="space-y-3">
            <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              {t('nodePanel.paramsTitle')}
            </h3>
            {specs.length === 0 ? (
              <p className="text-xs text-muted-foreground">{t('nodePanel.noParams')}</p>
            ) : (
              specs.map((spec) => (
                <ParamSpecField
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
  /** §6.8 管线级任务视图（仅已保存管线可用） */
  onShowTasks: () => void
  /** §6.4 TOML 导出（画布非空可用） */
  canExport: boolean
  onExport: () => void
  /** §6.4 TOML 导入 */
  onImport: () => void
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
  onShowTasks,
  canExport,
  onExport,
  onImport,
}: PipelineLibraryBarProps) {
  const { t } = useTranslation('pipeline')
  return (
    <div className="glass flex h-11 shrink-0 items-center gap-2 overflow-x-auto border-b border-border-glow px-3">
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
        <DropdownMenuContent
          align="start"
          className="max-h-96 w-80 overflow-y-auto border-border-glow shadow-[0_8px_24px_rgba(2,6,12,0.5)] backdrop-blur-xl"
        >
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
        {/* §6.8 管线级任务视图 / §6.4 TOML 分发 */}
        <Button
          variant="ghost"
          size="xs"
          onClick={onShowTasks}
          disabled={!currentId}
          title={t('ptasks.title', { defaultValue: '管线任务视图' })}
        >
          <History className="h-3.5 w-3.5" />
          <span className="hidden xl:inline">
            {t('ptasks.shortLabel', { defaultValue: '任务' })}
          </span>
        </Button>
        <Button
          variant="ghost"
          size="xs"
          onClick={onExport}
          disabled={!canExport}
          title={t('io.exportHint', { defaultValue: '导出当前画布为管线 TOML（含依赖清单）' })}
        >
          <Download className="h-3.5 w-3.5" />
          <span className="hidden xl:inline">{t('common:action.export')}</span>
        </Button>
        <Button
          variant="ghost"
          size="xs"
          onClick={onImport}
          title={t('io.importHint', { defaultValue: '上传管线 TOML 文件，校验并注册' })}
        >
          <Upload className="h-3.5 w-3.5" />
          <span className="hidden xl:inline">{t('common:action.import')}</span>
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
  /** 预填 ID（TOML 导入注册复用本对话框时传入文件内 [pipeline].id） */
  defaultId?: string
  pipelines: PipelineSummary[] | null
  /** 返回是否保存成功（失败时保持打开以便重试） */
  onConfirm: (name: string, id: string) => Promise<boolean>
}

/** 定时调度对话框：cron 表达式 + 启用开关；保存/移除走 /schedule API */
function ScheduleDialog({
  open,
  onOpenChange,
  pipelineId,
  form,
  onFormChange,
  current,
  onSave,
  onRemove,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  pipelineId: string | null
  form: { cron: string; enabled: boolean }
  onFormChange: (next: { cron: string; enabled: boolean }) => void
  current: { cron: string; enabled: boolean; last_task_id?: string | null } | null
  onSave: () => void
  onRemove: () => void
}) {
  const { t } = useTranslation('pipeline')
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            {t('schedule.title', { defaultValue: '定时调度' })}
            {pipelineId ? (
              <span className="ml-2 font-mono text-xs text-muted-foreground">{pipelineId}</span>
            ) : null}
          </DialogTitle>
          <DialogDescription>
            {t('schedule.description', {
              defaultValue:
                '按五段 cron（本地时区）周期自动执行本管线；留空任务输入时使用保存的模板。',
            })}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <label className="text-sm font-medium">
              {t('schedule.cron', { defaultValue: 'cron 表达式' })}
            </label>
            <Input
              value={form.cron}
              onChange={(e) =>
                onFormChange({ ...form, cron: e.target.value })
              }
              placeholder="0 3 * * *"
              className="font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">
              {t('schedule.cronHint', {
                defaultValue: '分 时 日 月 周；如 "0 3 * * *" 每天 03:00、"*/30 * * * *" 每 30 分钟',
              })}
            </p>
          </div>
          <div className="flex items-center justify-between rounded-lg border border-border px-3 py-2.5">
            <span className="text-sm">{t('schedule.enabled', { defaultValue: '启用' })}</span>
            <Switch checked={form.enabled} onCheckedChange={(v) => onFormChange({ ...form, enabled: v })} />
          </div>
          {current?.last_task_id ? (
            <p className="break-all font-mono text-[11px] text-muted-foreground">
              {t('schedule.lastTask', { defaultValue: '最近触发' })}: {current.last_task_id}
            </p>
          ) : null}
        </div>
        <DialogFooter className="gap-2">
          {current ? (
            <Button variant="ghost" size="sm" onClick={onRemove}>
              {t('schedule.remove', { defaultValue: '移除' })}
            </Button>
          ) : null}
          <Button size="sm" onClick={onSave} disabled={!form.cron.trim()}>
            {t('schedule.save', { defaultValue: '保存' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function SaveAsDialog({ open, onOpenChange, defaultName, defaultId, pipelines, onConfirm }: SaveAsDialogProps) {
  const { t } = useTranslation('pipeline')
  const [name, setName] = useState('')
  const [id, setId] = useState('')
  const [attempted, setAttempted] = useState(false)
  const [pending, setPending] = useState(false)

  useEffect(() => {
    if (open) {
      setName(defaultName)
      setId(defaultId ?? '')
      setAttempted(false)
      setPending(false)
    }
  }, [open, defaultName, defaultId])

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
// 执行对话框（收集 file_input 路径 + 补齐其他空必填参数
// + §6.5 无人值守选项：wait 同步模式 / callback_url 完成回调）
// ============================================================

/** 执行对话框高级选项（§6.5） */
interface ExecuteDialogOptions {
  /** 同步模式：阻塞至终态，响应直接带 status + artifacts */
  wait: boolean
  /** 完成回调 URL（可选，best-effort） */
  callbackUrl: string
}

interface ExecuteDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  fields: ExecField[]
  submitting: boolean
  /** values: key（`${nodeId}:${paramName}`）→ 参数值；opts: §6.5 高级选项 */
  onSubmit: (values: Record<string, ParamValue>, opts: ExecuteDialogOptions) => void
}

function ExecuteDialog({ open, onOpenChange, fields, submitting, onSubmit }: ExecuteDialogProps) {
  const { t } = useTranslation('pipeline')
  const [values, setValues] = useState<Record<string, ParamValue>>({})
  const [attempted, setAttempted] = useState(false)
  const [wait, setWait] = useState(false)
  const [callbackUrl, setCallbackUrl] = useState('')

  useEffect(() => {
    if (open) {
      const init: Record<string, ParamValue> = {}
      for (const f of fields) {
        if (f.current !== undefined && !isParamEmpty(f.current)) init[f.key] = f.current
      }
      setValues(init)
      setAttempted(false)
      setWait(false)
      setCallbackUrl('')
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
    onSubmit(values, { wait, callbackUrl: callbackUrl.trim() })
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
                      <ParamSpecField
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

            {/* §6.5 无人值守三件套之同步模式与完成回调 */}
            <section className="space-y-3 border-t border-border pt-3">
              <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                {t('execute.advancedTitle', { defaultValue: '高级选项（外部自动化 §6.5）' })}
              </h3>
              <div className="flex items-center justify-between gap-3 rounded-md border border-border bg-muted/30 px-3 py-2">
                <div className="min-w-0">
                  <p className="text-xs font-medium">
                    {t('execute.wait', { defaultValue: '同步模式（wait）' })}
                  </p>
                  <p className="text-[10px] text-muted-foreground">
                    {t('execute.waitHint', {
                      defaultValue: '阻塞至任务终态，响应直接带 status 与产物清单',
                    })}
                  </p>
                </div>
                <Switch checked={wait} onCheckedChange={setWait} />
              </div>
              <div className="space-y-1.5">
                <span className="text-xs font-medium">
                  {t('execute.callbackUrl', { defaultValue: '完成回调 URL（可选）' })}
                </span>
                <Input
                  value={callbackUrl}
                  onChange={(e) => setCallbackUrl(e.target.value)}
                  placeholder="https://watcher.example.com/ep/callback"
                  className="h-8 font-mono text-xs"
                />
                <p className="text-[10px] text-muted-foreground">
                  {t('execute.callbackHint', {
                    defaultValue: '任务终态时向该地址 POST {task_id, status, artifacts}（best-effort）',
                  })}
                </p>
              </div>
            </section>
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
// VRAM 每设备账本侧栏（§6.3：编辑器实时计算呈现面）
// ============================================================

interface VramLedgerPanelProps {
  report: VramBudgetResponse | null
  loading: boolean
  error: string | null
  /** 设备 id → 展示名（/api/devices） */
  deviceNames: Map<string, string>
  /** 节点 id → 画布标签（items 明细友好展示） */
  nodeLabels: Map<string, string>
  hasModuleNodes: boolean
  onRefresh: () => void
  onClose: () => void
  className?: string
}

function VramLedgerPanel({
  report,
  loading,
  error,
  deviceNames,
  nodeLabels,
  hasModuleNodes,
  onRefresh,
  onClose,
  className,
}: VramLedgerPanelProps) {
  const { t } = useTranslation('pipeline')
  const anyOver = report?.devices.some((d) => d.over) ?? false
  const blocked = !!report && !report.allow_overcommit && anyOver

  const labelFor = (nodeId: string) => nodeLabels.get(nodeId) ?? nodeId

  return (
    <aside
      className={cn(
        'glass flex h-full w-72 shrink-0 flex-col border-l border-border-glow',
        className,
      )}
    >
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-3">
        <MemoryStick className="h-4 w-4 text-primary" />
        <p className="min-w-0 flex-1 truncate text-sm font-semibold">
          {t('vram.title', { defaultValue: 'VRAM 每设备账本' })}
        </p>
        {loading && <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />}
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onRefresh}
          title={t('vram.refresh', { defaultValue: '重新估算' })}
          aria-label={t('vram.refresh', { defaultValue: '重新估算' })}
        >
          <RefreshCw className="h-3 w-3" />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          onClick={onClose}
          title={t('common:action.close')}
          aria-label={t('vram.closeAria', { defaultValue: '关闭 VRAM 账本' })}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      <ScrollArea className="flex-1">
        <div className="space-y-4 p-4">
          {error && (
            <p className="break-all rounded-md border border-status-error/30 bg-status-error/10 px-2.5 py-2 text-[11px] text-status-error">
              {t('vram.error', { defaultValue: 'VRAM 预算估算失败' })}：{error}
            </p>
          )}

          {!error && !hasModuleNodes && (
            <p className="text-xs text-muted-foreground">
              {t('vram.empty', { defaultValue: '画布没有模块节点，无需估算 VRAM。' })}
            </p>
          )}

          {!error && hasModuleNodes && !report && !loading && (
            <p className="text-xs text-muted-foreground">
              {t('vram.pending', { defaultValue: '正在等待首次估算…' })}
            </p>
          )}

          {report &&
            report.devices.map((d) => {
              const unknownCapacity = d.total_mb === null || d.total_mb === undefined
              const usedSafe = d.used_mb ?? 0
              const usedPct = unknownCapacity ? 0 : Math.min(100, (usedSafe / d.total_mb!) * 100)
              const pipePct = unknownCapacity
                ? 0
                : Math.min(Math.max(100 - usedPct, 0), (d.pipeline_mb / d.total_mb!) * 100)
              return (
                <div
                  key={d.device_id}
                  className={cn(
                    'space-y-2 rounded-lg border p-3',
                    d.over ? 'border-status-error/50 bg-status-error/5' : 'border-border-glow',
                  )}
                >
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-xs font-semibold">{d.device_id}</span>
                    {deviceNames.get(d.device_id) && (
                      <span className="min-w-0 flex-1 truncate text-[10px] text-muted-foreground">
                        {deviceNames.get(d.device_id)}
                      </span>
                    )}
                    {d.over && (
                      <Badge
                        variant="outline"
                        className="h-4 shrink-0 border-status-error/40 px-1 text-[9px] text-status-error"
                      >
                        {t('vram.over', { defaultValue: '超出预算' })}
                      </Badge>
                    )}
                  </div>

                  {/* 进度条：已用（灰）+ 管线需求（主色/超限红）；容量未知 → 未估算 */}
                  {unknownCapacity ? (
                    <div className="h-2 rounded-full bg-muted" />
                  ) : (
                    <div className="flex h-2 overflow-hidden rounded-full bg-muted">
                      <div
                        className="bg-muted-foreground/50"
                        style={{ width: `${usedPct}%` }}
                      />
                      <div
                        className={d.over ? 'bg-status-error' : 'bg-primary'}
                        style={{ width: `${pipePct}%` }}
                      />
                    </div>
                  )}
                  <p className="text-[10px] text-muted-foreground">
                    {unknownCapacity
                      ? t('vram.unknownCapacity', {
                          defaultValue: '设备内存未知 — 未估算（不计入超限判定）',
                        })
                      : t('vram.usageLine', {
                          defaultValue: '已用 {{used}} · 本管线峰值 {{pipeline}} / 总量 {{total}}',
                          used: d.used_mb === null ? '—' : formatMb(d.used_mb),
                          pipeline: formatMb(d.pipeline_mb),
                          total: formatMb(d.total_mb),
                        })}
                  </p>

                  {d.items.length > 0 && (
                    <ul className="space-y-0.5">
                      {d.items.map((item) => (
                        <li
                          key={item.node_id}
                          className="flex items-baseline justify-between gap-2 text-[10px]"
                        >
                          <span className="min-w-0 truncate text-muted-foreground">
                            {labelFor(item.node_id)}
                          </span>
                          <span className="shrink-0 font-mono">{formatMb(item.mb)}</span>
                        </li>
                      ))}
                    </ul>
                  )}

                  {d.over && (
                    <ul className="list-inside list-disc space-y-0.5 rounded-md bg-status-error/10 px-2.5 py-1.5 text-[10px] text-status-error">
                      <li>
                        {t('vram.adviceVariant', { defaultValue: '为该节点换用更小的模型变体' })}
                      </li>
                      <li>
                        {t('vram.adviceDevice', {
                          defaultValue: '将部分节点的设备绑定改到空闲的其他设备',
                        })}
                      </li>
                      <li>
                        {t('vram.adviceStop', { defaultValue: '停掉其他占用显存的模块' })}
                      </li>
                    </ul>
                  )}
                </div>
              )
            })}

          {/* auto 节点未分配池（由调度器按 least_memory 落位） */}
          {report && (report.unassigned_mb > 0 || report.unassigned.length > 0) && (
            <div className="space-y-2 rounded-lg border border-dashed border-border-glow p-3">
              <div className="flex items-center gap-2">
                <span className="text-xs font-semibold">
                  {t('vram.unassignedTitle', { defaultValue: '未分配（device=auto）' })}
                </span>
                <span className="ml-auto font-mono text-[11px]">
                  {formatMb(report.unassigned_mb)}
                </span>
              </div>
              <ul className="space-y-0.5">
                {report.unassigned.map((item) => (
                  <li
                    key={item.node_id}
                    className="flex items-baseline justify-between gap-2 text-[10px]"
                  >
                    <span className="min-w-0 truncate text-muted-foreground">
                      {labelFor(item.node_id)}
                    </span>
                    <span className="shrink-0 font-mono">{formatMb(item.mb)}</span>
                  </li>
                ))}
              </ul>
              <p className="text-[10px] text-muted-foreground">
                {t('vram.unassignedHint', {
                  defaultValue: 'auto 节点不计入设备账本，将由调度器按 least_memory 策略落位。',
                })}
              </p>
            </div>
          )}

          {report && (
            <p
              className={cn(
                'rounded-md px-2.5 py-2 text-[11px]',
                blocked
                  ? 'border border-status-error/30 bg-status-error/10 text-status-error'
                  : 'border border-border bg-muted/30 text-muted-foreground',
              )}
            >
              {blocked
                ? t('vram.blocked', {
                    defaultValue:
                      '存在超出预算的设备，且未允许超额提交（compute.allow_overcommit=false）— 执行已阻断，请按上方建议调整。',
                  })
                : report.allow_overcommit
                  ? t('vram.overcommitAllowed', {
                      defaultValue: 'allow_overcommit=true：超出预算仍将放行执行（可能 OOM）。',
                    })
                  : t('vram.overcommitDenied', {
                      defaultValue: 'allow_overcommit=false：超出预算时将阻断执行。',
                    })}
            </p>
          )}
        </div>
      </ScrollArea>
    </aside>
  )
}

// ============================================================
// 变体 pin 冲突对话框（§5.2 MVP：报错 + 一键切换引导，不静默热切换）
// ============================================================

interface VariantPinDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  issues: PinIssue[]
  /** 最新激活变体快照（切换成功后父级刷新） */
  activeModels: Record<string, string> | null
  /** 一键切换激活变体；返回是否成功 */
  onSwitch: (moduleId: string, variant: string) => Promise<boolean>
}

function VariantPinDialog({
  open,
  onOpenChange,
  issues,
  activeModels,
  onSwitch,
}: VariantPinDialogProps) {
  const { t } = useTranslation('pipeline')
  const [pendingKeys, setPendingKeys] = useState<Set<string>>(new Set())

  useEffect(() => {
    if (open) setPendingKeys(new Set())
  }, [open])

  const switchOne = async (issue: PinIssue) => {
    if (!issue.variant) return
    const key = `${issue.moduleId}@${issue.variant}`
    setPendingKeys((prev) => new Set(prev).add(key))
    try {
      await onSwitch(issue.moduleId, issue.variant)
    } finally {
      setPendingKeys((prev) => {
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
  }

  const mismatches = issues.filter((i) => i.type === 'mismatch')
  const others = issues.filter((i) => i.type !== 'mismatch')
  const allResolved =
    mismatches.length > 0 &&
    mismatches.every((i) => i.variant && activeModels?.[i.moduleId] === i.variant)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {t('pinCheck.title', { defaultValue: '变体 pin 与激活变体不一致' })}
          </DialogTitle>
          <DialogDescription>
            {t('pinCheck.description', {
              defaultValue:
                '管线节点 pin 了特定模型变体，与模块当前激活变体不一致。按 §5.2 不做静默热切换——请切换激活变体后再执行。',
            })}
          </DialogDescription>
        </DialogHeader>
        <ScrollArea className="max-h-[50vh]">
          <div className="space-y-3 p-1 pr-3">
            {mismatches.map((issue) => {
              const resolved = !!issue.variant && activeModels?.[issue.moduleId] === issue.variant
              const key = `${issue.moduleId}@${issue.variant}`
              return (
                <div
                  key={`${issue.nodeId}-${issue.pin}`}
                  className={cn(
                    'space-y-1.5 rounded-md border p-3',
                    resolved ? 'border-status-running/40 bg-status-running/5' : 'border-border-glow',
                  )}
                >
                  <div className="flex items-baseline gap-2">
                    <span className="truncate text-xs font-semibold">{issue.nodeLabel}</span>
                    <span className="truncate font-mono text-[10px] text-muted-foreground">
                      {issue.moduleId}
                    </span>
                  </div>
                  <p className="text-[11px] text-muted-foreground">
                    {t('pinCheck.pinnedTo', { defaultValue: 'pin' })}：
                    <span className="font-mono">{issue.pin}</span>
                    {' · '}
                    {t('pinCheck.activeIs', { defaultValue: '激活' })}：
                    <span className="font-mono">{issue.active ?? '—'}</span>
                  </p>
                  {resolved ? (
                    <p className="flex items-center gap-1 text-[11px] text-status-running">
                      <CircleCheck className="h-3 w-3" />
                      {t('pinCheck.resolved', { defaultValue: '已切换，pin 与激活变体一致' })}
                    </p>
                  ) : (
                    <Button
                      variant="outline"
                      size="xs"
                      disabled={pendingKeys.has(key)}
                      onClick={() => void switchOne(issue)}
                    >
                      {pendingKeys.has(key) ? (
                        <Loader2 className="h-3 w-3 animate-spin" />
                      ) : (
                        <ArrowRightLeft className="h-3 w-3" />
                      )}
                      {t('pinCheck.switch', { defaultValue: '切换激活变体到 pin' })}
                    </Button>
                  )}
                </div>
              )
            })}

            {others.map((issue) => (
              <div
                key={`${issue.nodeId}-${issue.pin}`}
                className="space-y-1 rounded-md border border-status-error/40 bg-status-error/5 p-3"
              >
                <div className="flex items-baseline gap-2">
                  <span className="truncate text-xs font-semibold">{issue.nodeLabel}</span>
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {issue.moduleId}
                  </span>
                </div>
                {issue.type === 'invalid' && (
                  <p className="text-[11px] text-status-error">
                    {t('pinCheck.invalidPin', {
                      defaultValue: 'pin 语法非法：{{pin}}（格式：publisher.vendor.model[@variant]）',
                      pin: issue.pin,
                    })}
                  </p>
                )}
                {issue.type === 'unknown_variant' && (
                  <p className="text-[11px] text-status-error">
                    {t('pinCheck.unknownVariant', {
                      defaultValue: '模块 {{moduleId}} 没有变体 {{variant}}，请到模型统一页下载或核对',
                      moduleId: issue.moduleId,
                      variant: issue.variant ?? '',
                    })}
                  </p>
                )}
              </div>
            ))}

            {allResolved && (
              <p className="rounded-md border border-status-running/40 bg-status-running/10 px-2.5 py-2 text-[11px] text-status-running">
                {t('pinCheck.allResolved', { defaultValue: '全部 pin 已与激活变体一致，关闭后可重新执行。' })}
              </p>
            )}
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t('common:action.close')}
          </Button>
          <Button variant="secondary" asChild>
            <Link to="/modules">
              {t('pinCheck.goModels', { defaultValue: '前往模块管理' })}
            </Link>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ============================================================
// 管线级任务视图对话框（§6.8：执行历史/在跑任务 + 队列位置 + 产物下载）
// ============================================================

/** 任务状态徽章元信息（queued 为 §6.8 新增状态） */
const PIPELINE_TASK_STATE_META: Record<
  string,
  { labelKey: string; fallback: string; badge: string; pulse: boolean }
> = {
  queued: {
    labelKey: 'ptasks.statusQueued',
    fallback: '排队中',
    badge: 'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
    pulse: true,
  },
  running: {
    labelKey: 'common:status.running',
    fallback: '运行中',
    badge: 'bg-status-starting/15 text-status-starting border-status-starting/30',
    pulse: true,
  },
  completed: {
    labelKey: 'common:status.completed',
    fallback: '已完成',
    badge: 'bg-status-running/15 text-status-running border-status-running/30',
    pulse: false,
  },
  failed: {
    labelKey: 'common:status.failed',
    fallback: '失败',
    badge: 'bg-status-error/15 text-status-error border-status-error/30',
    pulse: false,
  },
  cancelled: {
    labelKey: 'common:status.cancelled',
    fallback: '已取消',
    badge: 'bg-status-preparing/15 text-status-preparing border-status-preparing/30',
    pulse: false,
  },
}

function pipelineTaskStateMeta(status: string) {
  const meta = PIPELINE_TASK_STATE_META[status.trim().toLowerCase()]
  return (
    meta ?? {
      labelKey: null as string | null,
      fallback: status,
      badge: 'bg-muted text-muted-foreground border-border',
      pulse: false,
    }
  )
}

function formatTaskTime(iso: string | undefined, language: string): string {
  if (!iso) return '—'
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return '—'
  const fmt = new Intl.DateTimeFormat(language.startsWith('zh') ? 'zh-CN' : 'en', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
  return fmt.format(date)
}

interface PipelineTasksDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  pipelineId: string | null
}

function PipelineTasksDialog({ open, onOpenChange, pipelineId }: PipelineTasksDialogProps) {
  const { t, i18n } = useTranslation('pipeline')
  const [tasks, setTasks] = useState<TaskSummary[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [artifacts, setArtifacts] = useState<
    Record<string, TaskArtifact[] | 'loading' | 'error'>
  >({})

  const load = useCallback(() => {
    if (!pipelineId) return
    api
      .pipelineTasks(pipelineId, { limit: 50 })
      .then((list) => {
        setTasks(list)
        setError(null)
      })
      .catch((e) => setError(errMsg(e)))
  }, [pipelineId])

  useEffect(() => {
    if (!open || !pipelineId) return
    setTasks(null)
    setError(null)
    setExpandedId(null)
    setArtifacts({})
    load()
  }, [open, pipelineId, load])

  // 存在非终态任务时轮询刷新（队列位置/进度可见性，§6.8）
  useEffect(() => {
    if (!open || !pipelineId) return
    const timer = window.setInterval(() => {
      const active = tasks?.some((task) =>
        ['queued', 'running', 'starting', 'preparing'].includes(
          task.status.trim().toLowerCase(),
        ),
      )
      if (active || tasks === null) load()
    }, 3000)
    return () => window.clearInterval(timer)
  }, [open, pipelineId, tasks, load])

  const toggleExpand = (task: TaskSummary) => {
    const next = expandedId === task.id ? null : task.id
    setExpandedId(next)
    if (next && task.status === 'completed' && artifacts[task.id] === undefined) {
      setArtifacts((prev) => ({ ...prev, [task.id]: 'loading' }))
      api
        .listTaskArtifacts(task.id)
        .then((list) => setArtifacts((prev) => ({ ...prev, [task.id]: list })))
        .catch(() => setArtifacts((prev) => ({ ...prev, [task.id]: 'error' })))
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('ptasks.title', { defaultValue: '管线任务视图' })}</DialogTitle>
          <DialogDescription>
            {pipelineId ?? ''} ·{' '}
            {t('ptasks.description', {
              defaultValue: '该管线的执行历史与在跑任务（§6.8，含队列位置）',
            })}
          </DialogDescription>
        </DialogHeader>
        <ScrollArea className="max-h-[55vh]">
          <div className="space-y-2 p-1 pr-3">
            {error && (
              <p className="rounded-md border border-status-error/30 bg-status-error/10 px-2.5 py-2 text-[11px] text-status-error">
                {t('ptasks.loadFailed', { defaultValue: '任务列表加载失败' })}：{error}
              </p>
            )}
            {!error && tasks === null && (
              <p className="flex items-center gap-2 py-6 text-xs text-muted-foreground">
                <Loader2 className="h-3 w-3 animate-spin" />
                {t('library.loading')}
              </p>
            )}
            {!error && tasks !== null && tasks.length === 0 && (
              <p className="py-6 text-center text-xs text-muted-foreground">
                {t('ptasks.empty', { defaultValue: '该管线暂无任务记录' })}
              </p>
            )}
            {tasks?.map((task) => {
              const meta = pipelineTaskStateMeta(task.status)
              const expanded = expandedId === task.id
              const arts = artifacts[task.id]
              return (
                <div key={task.id} className="rounded-md border border-border-glow transition-colors duration-150 hover:border-border-glow-strong">
                  <button
                    type="button"
                    className="flex w-full items-center gap-2 px-3 py-2 text-left"
                    onClick={() => toggleExpand(task)}
                  >
                    <ChevronDown
                      className={cn(
                        'h-3 w-3 shrink-0 text-muted-foreground transition-transform',
                        !expanded && '-rotate-90',
                      )}
                    />
                    <span
                      className={cn(
                        'inline-flex h-4 shrink-0 items-center gap-1 rounded border px-1 text-[9px]',
                        meta.badge,
                      )}
                    >
                      {meta.pulse && <span className="h-1 w-1 animate-pulse rounded-full bg-current" />}
                      {meta.labelKey
                        ? t(meta.labelKey, { defaultValue: meta.fallback })
                        : meta.fallback}
                    </span>
                    <span className="min-w-0 truncate font-mono text-[10px] text-muted-foreground">
                      {task.id}
                    </span>
                    {typeof task.queue_position === 'number' && (
                      <Badge
                        variant="outline"
                        className="h-4 shrink-0 px-1 text-[9px] text-status-preparing"
                      >
                        {t('ptasks.queuePosition', {
                          defaultValue: '队列 #{{pos}}',
                          pos: task.queue_position,
                        })}
                      </Badge>
                    )}
                    <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
                      {t('ptasks.progress', {
                        defaultValue: '{{done}}/{{count}} 节点',
                        done: task.completed_nodes,
                        count: task.node_count,
                      })}
                      {' · '}
                      {formatTaskTime(task.started_at, i18n.language)}
                    </span>
                  </button>
                  {expanded && (
                    <div className="space-y-1.5 border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
                      {task.error && (
                        <p className="break-all text-status-error">{task.error}</p>
                      )}
                      <p>
                        {t('ptasks.timeDetail', {
                          defaultValue: '提交 {{started}} · 开始运行 {{running}} · 结束 {{finished}}',
                          started: formatTaskTime(task.started_at, i18n.language),
                          running: formatTaskTime(task.started_running_at, i18n.language),
                          finished: formatTaskTime(task.finished_at, i18n.language),
                        })}
                      </p>
                      {task.status === 'completed' && (
                        <div className="space-y-1">
                          <p className="font-medium text-foreground">
                            {t('ptasks.artifacts', { defaultValue: '产物' })}
                          </p>
                          {arts === 'loading' && <Loader2 className="h-3 w-3 animate-spin" />}
                          {arts === 'error' && (
                            <p className="text-status-error">
                              {t('ptasks.artifactsFailed', { defaultValue: '产物列表加载失败' })}
                            </p>
                          )}
                          {Array.isArray(arts) && arts.length === 0 && (
                            <p>{t('ptasks.artifactsEmpty', { defaultValue: '无产物' })}</p>
                          )}
                          {Array.isArray(arts) &&
                            arts.map((artifact) => (
                              <a
                                key={artifact.node_id}
                                href={api.taskArtifactUrl(task.id, artifact.node_id)}
                                className="flex items-center gap-1.5 rounded border border-border px-2 py-1 font-mono text-[10px] text-foreground transition-colors hover:bg-accent"
                              >
                                <Download className="h-3 w-3" />
                                {artifact.name || artifact.node_id}
                                <span className="ml-auto text-muted-foreground">
                                  {artifact.size >= 1024 * 1024
                                    ? formatMb(Math.round(artifact.size / (1024 * 1024)))
                                    : `${Math.max(1, Math.round(artifact.size / 1024))} KB`}
                                </span>
                              </a>
                            ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={load}>
            <RefreshCw className="h-3.5 w-3.5" />
            {t('common:action.refresh')}
          </Button>
          <Button onClick={() => onOpenChange(false)}>{t('common:action.close')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ============================================================
// TOML 导入预览对话框（§6.4：校验 + 缺模型/变体列表提示 → PUT 注册）
// ============================================================

/** 导入的 TOML 解析结果 + 依赖提示 */
interface ImportDraft {
  fileName: string
  spec: PipelineSpec
  issues: ImportIssue[]
}

interface ImportTomlDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  draft: ImportDraft | null
  pipelines: PipelineSummary[] | null
  /** 注册（PUT /api/pipelines/:id）；返回是否成功 */
  onConfirm: (name: string, id: string) => Promise<boolean>
}

function ImportTomlDialog({
  open,
  onOpenChange,
  draft,
  pipelines,
  onConfirm,
}: ImportTomlDialogProps) {
  const { t } = useTranslation('pipeline')
  const [name, setName] = useState('')
  const [id, setId] = useState('')
  const [attempted, setAttempted] = useState(false)
  const [pending, setPending] = useState(false)

  useEffect(() => {
    if (open && draft) {
      setName(draft.spec.pipeline.name)
      setId(draft.spec.pipeline.id)
      setAttempted(false)
      setPending(false)
    }
  }, [open, draft])

  if (!draft) return null

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
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('io.importTitle', { defaultValue: '导入管线 TOML' })}</DialogTitle>
          <DialogDescription>
            {t('io.importDescription', {
              defaultValue: '文件解析成功，核对信息后注册到服务端管线库。',
            })}
          </DialogDescription>
        </DialogHeader>
        <ScrollArea className="max-h-[55vh]">
          <div className="space-y-4 p-1 pr-3">
            <div className="space-y-1 rounded-md border border-border bg-muted/30 px-3 py-2 text-[11px] text-muted-foreground">
              <p className="font-mono">{draft.fileName}</p>
              <p>
                {t('io.importStats', {
                  defaultValue: '{{nodes}} 个节点 · {{edges}} 条连接',
                  nodes: draft.spec.nodes.length,
                  edges: draft.spec.edges.length,
                })}
              </p>
            </div>

            {draft.issues.length > 0 && (
              <div className="space-y-1.5">
                <h3 className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                  {t('io.issuesTitle', { defaultValue: '依赖提示' })}
                </h3>
                {draft.issues.map((issue, index) => (
                  <p
                    key={index}
                    className={cn(
                      'flex items-start gap-1.5 rounded-md border px-2.5 py-2 text-[11px]',
                      issue.level === 'warn'
                        ? 'border-status-starting/30 bg-status-starting/10 text-status-starting'
                        : 'border-border bg-muted/30 text-muted-foreground',
                    )}
                  >
                    <TriangleAlert className="mt-px h-3 w-3 shrink-0" />
                    <span>{issue.text}</span>
                  </p>
                ))}
                <p className="text-[10px] text-muted-foreground">
                  {t('io.issuesHint', {
                    defaultValue: '以上问题不阻断注册；缺失的模型/变体可到模型统一页下载或切换。',
                  })}{' '}
                  <Link
                    to="/modules"
                    className="text-primary underline-offset-4 hover:underline"
                  >
                    {t('pinCheck.goModels', { defaultValue: '前往模块管理' })}
                  </Link>
                </p>
              </div>
            )}

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
                className="h-8 text-sm"
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
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
            {t('common:action.cancel')}
          </Button>
          <Button onClick={handleSubmit} disabled={pending}>
            {pending && <Loader2 className="animate-spin" />}
            {t('io.register', { defaultValue: '注册管线' })}
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
  const [scheduleOpen, setScheduleOpen] = useState(false)
  const [scheduleForm, setScheduleForm] = useState({ cron: '0 3 * * *', enabled: true })
  const [scheduleCurrent, setScheduleCurrent] = useState<
    | { cron: string; enabled: boolean; last_task_id?: string | null } | null
  >(null)
  const [currentSource, setCurrentSource] = useState<'builtin' | 'custom' | null>(null)
  /** 上次加载 / 保存时的画布指纹，用于判定未保存更改 */
  const [baseline, setBaseline] = useState(() =>
    canvasFingerprint([], [], { id: '', name: t('canvas.untitledName'), description: '' }),
  )
  const [saveAsOpen, setSaveAsOpen] = useState(false)

  // ---- 执行状态 ----
  // 执行锁基于**任务终态**而非 WS 连接（修 P2-7 后半：断连不永久锁死）：
  // WS progress 按当前执行 task_id 过滤驱动节点状态（快路径），
  // 同时轮询 GET /api/tasks/:id 直至终态（HTTP 路径不受 WS 断连影响）。
  const [executing, setExecuting] = useState(false)
  const executingRef = useRef(false)
  const [execDialogOpen, setExecDialogOpen] = useState(false)
  const [execFields, setExecFields] = useState<ExecField[]>([])
  const [execSubmitting, setExecSubmitting] = useState(false)
  /** 当前执行任务 id（WS task_id 过滤 + 终态轮询；null = 无进行中执行） */
  const activeTaskIdRef = useRef<string | null>(null)
  const pollErrorsRef = useRef(0)

  // ---- 设备 / 模块 / 模型 / 激活变体（§6.2/§6.3/§5.2 数据源） ----
  const [devices, setDevices] = useState<DeviceResponse[]>([])
  const [moduleList, setModuleList] = useState<ModuleResponse[] | null>(null)
  const [modelsList, setModelsList] = useState<ModelListResponse | null>(null)
  const [activeModels, setActiveModels] = useState<Record<string, string> | null>(null)

  // ---- VRAM 每设备账本（§6.3） ----
  const [vramOpen, setVramOpen] = useState(false)
  const [vramReport, setVramReport] = useState<VramBudgetResponse | null>(null)
  const [vramError, setVramError] = useState<string | null>(null)
  const [vramLoading, setVramLoading] = useState(false)
  const vramSeqRef = useRef(0)

  // ---- 对话框开关 ----
  const [pinDialogOpen, setPinDialogOpen] = useState(false)
  const [pinIssues, setPinIssues] = useState<PinIssue[]>([])
  const [tasksDialogOpen, setTasksDialogOpen] = useState(false)
  const [importDraft, setImportDraft] = useState<ImportDraft | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const importFileRef = useRef<HTMLInputElement>(null)

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

  /** §6.2 绑定字段更新（model/device；空串归一为 undefined = 跟随缺省语义） */
  const updateNodeBinding = useCallback(
    (nodeId: string, patch: ModuleBindingExt) => {
      setNodes((nds) =>
        nds.map((n) => {
          if (n.id !== nodeId || n.data.kind !== 'module') return n
          const data: ModuleNodeData = { ...n.data }
          if (patch.model !== undefined) {
            data.model = patch.model.trim() || undefined
          }
          if (patch.device !== undefined) {
            data.device = patch.device.trim() || undefined
          }
          return { ...n, data } as PipelineFlowNode
        }),
      )
    },
    [setNodes],
  )

  /** 能力切换（C2 CapabilitySelect）：按新能力的 params 重建默认参数 */
  const changeNodeCapability = useCallback(
    (nodeId: string, capabilityId: string) => {
      setNodes((nds) =>
        nds.map((n) => {
          if (n.id !== nodeId || n.data.kind !== 'module') return n
          const cap = (n.data.capabilities ?? []).find((c) => c.id === capabilityId)
          if (!cap) return n
          return {
            ...n,
            data: {
              ...n.data,
              capabilityId: cap.id,
              capabilityLabel: cap.label,
              params: defaultParams(cap.params),
            },
          } as PipelineFlowNode
        }),
      )
    },
    [setNodes],
  )

  /** 所选模块节点的变体列表与激活变体（NodeParamsPanel / ModuleBindingEditor 数据） */
  const selectedModuleId =
    selectedNode && selectedNode.data.kind === 'module' ? selectedNode.data.moduleId : null

  const selectedModuleVariants = useMemo(() => {
    if (!selectedModuleId) return []
    return (
      modelsList?.modules
        .find((m) => m.module_id === selectedModuleId)
        ?.models.map((m) => m.model_id) ?? []
    )
  }, [selectedModuleId, modelsList])

  const selectedModuleActiveVariant = useMemo(() => {
    if (!selectedModuleId) return undefined
    return activeModels?.[selectedModuleId]
  }, [selectedModuleId, activeModels])

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
  // P2-7：progress 消息按当前执行的 task_id 过滤，消除多管线并发的状态串染。
  // 无进行中执行（activeTaskId=null）时忽略全部 progress；消息缺 task_id
  // （后端过渡期）时仅在确实执行中才接受（向后兼容）。

  useEffect(() => {
    return wsManager.onMessage((msg) => {
      if (msg.type !== 'progress') return
      const activeTaskId = activeTaskIdRef.current
      if (!activeTaskId) return
      const taskId = typeof msg.task_id === 'string' && msg.task_id ? msg.task_id : null
      if (taskId && taskId !== activeTaskId) return
      const nodeId = typeof msg.node_id === 'string' ? msg.node_id : null
      if (!nodeId) return
      const status = normalizeNodeStatus(typeof msg.status === 'string' ? msg.status : null)
      // 函数式更新：基于最新节点集映射，避免与拖动等操作同 tick 时
      // 用 nodesRef 快照覆盖丢失节点位置（P2）
      setNodes((nds) => {
        if (!nds.some((n) => n.id === nodeId)) return nds
        const next = nds.map((n) =>
          n.id === nodeId ? ({ ...n, data: { ...n.data, status } } as PipelineFlowNode) : n,
        )
        // 快路径解锁：任一节点失败（管线中止）或全部节点到达终态。
        // 权威解锁走下方任务终态轮询（WS 断连也不永久锁死）。
        if (
          executingRef.current &&
          (status === 'failed' ||
            next.every((n) => n.data.status === 'done' || n.data.status === 'failed'))
        ) {
          executingRef.current = false
          setExecuting(false)
          activeTaskIdRef.current = null
        }
        return next
      })
    })
  }, [setNodes])

  // ---- 任务终态轮询（执行锁权威来源：HTTP 路径不受 WS 断连影响） ----

  useEffect(() => {
    if (!executing) return
    const timer = window.setInterval(() => {
      const taskId = activeTaskIdRef.current
      if (!taskId) return
      api
        .getTask(taskId)
        .then((detail: TaskDetail) => {
          pollErrorsRef.current = 0
          const status = (detail.status ?? '').trim().toLowerCase()
          if (status !== 'completed' && status !== 'failed' && status !== 'cancelled') return
          // 终态：解锁 + 以任务注册表的节点状态同步画布（比 WS 更权威）
          executingRef.current = false
          setExecuting(false)
          activeTaskIdRef.current = null
          if (Array.isArray(detail.nodes)) {
            const stateByNode = new Map(detail.nodes.map((nr) => [nr.node_id, nr.state]))
            setNodes((nds) =>
              nds.map((n) => {
                const state = stateByNode.get(n.id)
                if (state === undefined) return n
                return {
                  ...n,
                  data: { ...n.data, status: normalizeNodeStatus(state) },
                } as PipelineFlowNode
              }),
            )
          }
        })
        .catch(() => {
          // daemon 暂不可达：连续失败超过阈值后解锁并提示，避免永久锁死；
          // 否则保持锁（任务真实状态未知，防重复提交）
          pollErrorsRef.current += 1
          if (pollErrorsRef.current >= 8) {
            executingRef.current = false
            setExecuting(false)
            activeTaskIdRef.current = null
            toast.warning(
              t('exec.pollLost', {
                defaultValue: '无法查询任务状态，执行锁已释放',
              }),
              {
                description: t('exec.pollLostHint', {
                  defaultValue: '任务可能仍在后台运行，请到任务中心确认后再决定是否重新执行。',
                }),
              },
            )
          }
        })
    }, 2500)
    return () => window.clearInterval(timer)
  }, [executing, setNodes, t])

  // ---- 设备 / 模块 / 模型 / 激活变体加载 ----

  const refreshDevices = useCallback(() => {
    api
      .devices()
      .then(setDevices)
      .catch(() => {})
  }, [])

  const refreshActiveModels = useCallback(async () => {
    try {
      const cfg = await api.getConfig()
      setActiveModels(cfg.active_models ?? {})
    } catch {
      // 配置不可达时 pin 校验跳过 mismatch 判定（保守：不误报）
    }
  }, [])

  useEffect(() => {
    refreshDevices()
    refreshActiveModels()
    api
      .modules()
      .then(setModuleList)
      .catch(() => {})
    api
      .models()
      .then(setModelsList)
      .catch(() => {})
  }, [refreshDevices, refreshActiveModels])

  // 模块列表异步到达后水合画布模块节点的能力/分类/版本（不改契约字段，
  // canvasFingerprint 不受影响，不会误报未保存更改）
  useEffect(() => {
    if (!moduleList) return
    setNodes((nds) => {
      let changed = false
      const next = nds.map((n) => {
        if (n.data.kind !== 'module') return n
        const nodeId = n.data.moduleId
        const m = moduleList.find((mm) => mm.id === nodeId)
        if (!m) return n
        const caps = capabilitiesFromModule(m.capabilities)
        const data = n.data as ModuleNodeData
        const cap = caps.find((c) => c.id === data.capabilityId) ?? caps[0] ?? null
        changed = true
        return {
          ...n,
          data: {
            ...data,
            capabilities: caps,
            category: m.category,
            moduleVersion: m.version,
            capabilityId: cap?.id ?? data.capabilityId,
            capabilityLabel: cap?.label ?? data.capabilityLabel,
          },
        } as PipelineFlowNode
      })
      return changed ? next : nds
    })
  }, [moduleList, setNodes])

  // ---- VRAM 预算：画布编辑 → 防抖 500ms 调 vram-budget（§6.3） ----

  const hasModuleNodes = useMemo(() => nodes.some((n) => n.data.kind === 'module'), [nodes])

  const fetchVramBudget = useCallback(() => {
    const seq = ++vramSeqRef.current
    const spec = buildBudgetSpec(nodes, edges)
    if (!spec) {
      setVramReport(null)
      setVramError(null)
      setVramLoading(false)
      return
    }
    setVramLoading(true)
    api
      .vramBudget({ spec })
      .then((report) => {
        if (vramSeqRef.current !== seq) return
        setVramReport(report)
        setVramError(null)
      })
      .catch((e) => {
        if (vramSeqRef.current !== seq) return
        setVramReport(null)
        setVramError(errMsg(e))
      })
      .finally(() => {
        if (vramSeqRef.current === seq) setVramLoading(false)
      })
  }, [nodes, edges])

  useEffect(() => {
    if (!hasModuleNodes) {
      vramSeqRef.current += 1
      setVramReport(null)
      setVramError(null)
      setVramLoading(false)
      return
    }
    const timer = window.setTimeout(fetchVramBudget, 500)
    return () => window.clearTimeout(timer)
  }, [nodes, edges, hasModuleNodes, fetchVramBudget])

  const deviceNames = useMemo(
    () => new Map(devices.map((d) => [d.id, d.name])),
    [devices],
  )
  const nodeLabels = useMemo(
    () => new Map(nodes.map((n) => [n.id, n.data.label])),
    [nodes],
  )

  /**
   * 执行阻断（§6.3）：allow_overcommit=false 且任一设备 over → 禁止执行。
   * 返回被阻断的设备列表文案；null = 放行。
   */
  const vramBlockedBy = useMemo((): string | null => {
    if (!vramReport || vramReport.allow_overcommit) return null
    const overs = vramReport.devices.filter((d) => d.over).map((d) => d.device_id)
    if (overs.length === 0) return null
    return overs.join(', ')
  }, [vramReport])

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

  /** 从服务端拉取 spec 并铺到画布（不含丢弃确认；调用方自行处理） */
  const loadServerPipeline = useCallback(
    async (id: string) => {
      const toastId = toast.loading(t('load.loading'))
      try {
        const spec = await api.getPipeline(id)
        const { nodes: loadedNodes, edges: loadedEdges, skippedNodes } = fromSpec(
          spec,
          moduleList,
        )
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
    [moduleList, pipelines, setNodes, setEdges, fitSoon, t],
  )

  /** 选择服务端管线 → 加载 spec 并铺到画布 */
  const handleSelectPipeline = useCallback(
    async (id: string) => {
      if (!(await confirmDiscardIfDirty())) return
      await loadServerPipeline(id)
    },
    [confirmDiscardIfDirty, loadServerPipeline],
  )

  /** 新建空白管线（file_input → file_output 最小模板） */
  const handleNewBlank = useCallback(async () => {
    if (!(await confirmDiscardIfDirty())) return
    const { nodes: n, edges: e } = blankTemplate()
    resetCanvas(n, e, t('canvas.untitledName'), '')
    fitSoon()
    toast.success(t('template.blankCreated'), { description: t('template.blankDescription') })
  }, [confirmDiscardIfDirty, resetCanvas, fitSoon, t])

  /** 载入本地示例模板（深拷贝，避免画布编辑污染模板；能力数据驱动） */
  const handleLoadExample = useCallback(async () => {
    if (!(await confirmDiscardIfDirty())) return
    // 每次调用时构建，确保模板节点标签取当前语言的文案
    const cloned = structuredClone(examplePipeline(moduleList))
    resetCanvas(cloned.nodes, cloned.edges, t('canvas.exampleName'), '')
    fitSoon()
    toast.success(t('template.exampleLoaded'), { description: t('template.exampleDescription') })
  }, [confirmDiscardIfDirty, moduleList, resetCanvas, fitSoon, t])

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

  // ---- 管线 TOML 导出 / 导入（§6.4） ----

  /** 导出当前画布为管线 TOML（含头部注释依赖清单）并触发下载 */
  const handleExportToml = useCallback(() => {
    if (nodes.length === 0) {
      toast.info(t('io.exportEmpty', { defaultValue: '画布为空，无法导出' }))
      return
    }
    try {
      const spec = toSpec(nodes, edges, {
        id: currentId ?? fallbackPipelineId(name),
        name,
        description,
      })
      const toml = specToToml(spec, collectDependencyLines(nodes))
      const blob = new Blob([toml], { type: 'text/plain;charset=utf-8' })
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = `${spec.pipeline.id}.toml`
      anchor.click()
      window.setTimeout(() => URL.revokeObjectURL(url), 1000)
      toast.success(t('io.exportSuccess', { defaultValue: '管线 TOML 已导出' }), {
        description: t('io.exportStats', {
          defaultValue: '{{nodes}} 个节点 · 依赖 {{deps}} 项',
          nodes: spec.nodes.length,
          deps: collectDependencyLines(nodes).length,
        }),
      })
    } catch (err) {
      toast.error(t('io.exportFailed', { defaultValue: '导出失败' }), {
        description: errMsg(err),
      })
    }
  }, [nodes, edges, currentId, name, description, t])

  /** 选择 TOML 文件 → 解析 + 依赖校验 → 打开导入预览对话框 */
  const handleImportFile = useCallback(
    async (file: File) => {
      if (!file.name.toLowerCase().endsWith('.toml')) {
        toast.error(t('io.invalidFile', { defaultValue: '请选择 .toml 管线文件' }))
        return
      }
      try {
        const text = await file.text()
        const spec = parsePipelineToml(text)
        const issues = collectImportIssues(spec, moduleList, modelsList, t)
        setImportDraft({ fileName: file.name, spec, issues })
        setImportOpen(true)
      } catch (err) {
        toast.error(t('io.importParseFailed', { defaultValue: 'TOML 解析失败' }), {
          description: errMsg(err),
        })
      }
    },
    [moduleList, modelsList, t],
  )

  /** 导入确认：PUT 注册 → 刷新列表 → 加载到画布 */
  const handleImportConfirm = useCallback(
    async (id: string, importName: string): Promise<boolean> => {
      if (!importDraft) return false
      const spec: PipelineSpec = {
        ...importDraft.spec,
        pipeline: { ...importDraft.spec.pipeline, id, name: importName },
      }
      const toastId = toast.loading(t('io.registering', { defaultValue: '注册中…' }))
      try {
        await api.savePipeline(id, spec)
        refreshPipelines()
        toast.success(t('io.registerSuccess', { defaultValue: '管线已注册' }), { id: toastId })
        await loadServerPipeline(id)
        return true
      } catch (err) {
        toast.error(t('io.registerFailed', { defaultValue: '注册失败' }), {
          id: toastId,
          description: errMsg(err),
        })
        return false
      }
    },
    [importDraft, refreshPipelines, loadServerPipeline, t],
  )

  // ---- 变体 pin 校验与一键切换（§5.2 MVP） ----

  /** 一键切换激活变体到 pin（B5 PUT variant）；返回是否成功 */
  const switchVariantToPin = useCallback(
    async (moduleId: string, variant: string): Promise<boolean> => {
      const toastId = toast.loading(
        t('pinCheck.switching', { defaultValue: '切换激活变体中…' }),
      )
      try {
        const resp = await api.setModelVariant(moduleId, variant, { model_id: variant })
        await refreshActiveModels()
        if (resp.needs_download) {
          toast.warning(
            t('pinCheck.needsDownload', {
              defaultValue: '已切换，但目标变体本地缺失 — 请到模型统一页下载',
            }),
            { id: toastId },
          )
        } else if (resp.needs_restart) {
          toast.info(
            t('pinCheck.needsRestart', {
              defaultValue: '已切换 — 模块运行中，重启模块后生效',
            }),
            { id: toastId },
          )
        } else {
          toast.success(t('pinCheck.switched', { defaultValue: '激活变体已切换' }), {
            id: toastId,
          })
        }
        return true
      } catch (err) {
        toast.error(t('pinCheck.switchFailed', { defaultValue: '切换失败' }), {
          id: toastId,
          description: errMsg(err),
        })
        return false
      }
    },
    [refreshActiveModels, t],
  )

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

  /** 导出当前服务端管线为分享 JSON（触发浏览器下载） */
  const handleExportCurrent = useCallback(() => {
    if (!currentId) return
    const a = document.createElement('a')
    a.href = api.exportPipelineUrl(currentId)
    a.download = `${currentId}.pipeline.json`
    document.body.appendChild(a)
    a.click()
    a.remove()
    toast.success(t('share.exported', { defaultValue: '已导出分享 JSON' }), {
      description: `${currentId}.pipeline.json`,
    })
  }, [currentId, t])

  /** 导入分享 JSON → 服务端直接建管线 → 选入编辑器 */
  const handleImportShare = useCallback(
    async (file: File) => {
      try {
        const text = await file.text()
        const res = await api.importPipelineShare(text)
        toast.success(t('share.imported', { defaultValue: '导入成功' }), {
          description: `${res.name} (${res.id})`,
        })
        refreshPipelines()
        void handleSelectPipeline(res.id)
      } catch (err) {
        toast.error(t('share.importFailed', { defaultValue: '导入失败' }), {
          description: errMsg(err),
        })
      }
    },
    // handleSelectPipeline 内部稳定引用；refreshPipelines 来自库刷新 hook
    [refreshPipelines, t],
  )

  /** 打开定时对话框时拉取当前计划 */
  const openScheduleDialog = useCallback(async () => {
    if (!currentId) return
    setScheduleForm({ cron: '0 3 * * *', enabled: true })
    setScheduleCurrent(null)
    setScheduleOpen(true)
    try {
      const info = await api.getSchedule(currentId)
      setScheduleCurrent(info.schedule)
      setScheduleForm({ cron: info.schedule.cron, enabled: info.schedule.enabled })
    } catch {
      // 未配置过 → 保持缺省表单
    }
  }, [currentId])

  const saveSchedule = useCallback(async () => {
    if (!currentId) return
    try {
      await api.putSchedule(currentId, scheduleForm)
      toast.success(t('schedule.saved', { defaultValue: '定时计划已保存' }))
      setScheduleOpen(false)
    } catch (err) {
      toast.error(t('schedule.saveFailed', { defaultValue: '保存失败' }), {
        description: errMsg(err),
      })
    }
  }, [currentId, scheduleForm, t])

  const removeSchedule = useCallback(async () => {
    if (!currentId) return
    try {
      await api.deleteSchedule(currentId)
      toast.success(t('schedule.removed', { defaultValue: '定时计划已移除' }))
      setScheduleOpen(false)
    } catch (err) {
      toast.error(t('schedule.saveFailed', { defaultValue: '保存失败' }), {
        description: errMsg(err),
      })
    }
  }, [currentId, t])

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
      // §5.6：file_gate 静态校验（与后端 validate() 同语义）——
      // 扩展名白/黑名单、大小界、文件名正则、media 条件至少配置一项
      if (n.data.kind === 'builtin' && n.data.builtin === 'file_gate') {
        const p = n.data.params
        const hasCondition =
          (Array.isArray(p.extensions) && p.extensions.some((s) => s.trim() !== '')) ||
          (Array.isArray(p.extensions_exclude) && p.extensions_exclude.some((s) => s.trim() !== '')) ||
          (typeof p.filename_regex === 'string' && p.filename_regex.trim() !== '') ||
          [
            p.min_size_bytes,
            p.max_size_bytes,
            p.media_min_duration_secs,
            p.media_max_duration_secs,
            p.media_min_width,
            p.media_min_height,
          ].some((v) => typeof v === 'number' && Number.isFinite(v) && v > 0)
        if (!hasCondition) {
          issues.push(t('validate.fileGateNoCondition', { label: n.data.label }))
        }
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
   * 执行前置校验：VRAM 超限阻断（§6.3）+ 变体 pin 校验（§5.2）+
   * 连线完整性 + 必填参数校验（P1-21）+ file_input 存在性。
   * 空的必填参数不在此阻断，而是弹执行对话框补齐。
   */
  const handleExecute = useCallback(() => {
    if (executing) {
      toast.info(t('exec.alreadyRunning'), { description: t('exec.waitFinish') })
      return
    }
    // §6.3：allow_overcommit=false 且存在 over 设备 → 阻断执行并给出原因
    if (vramBlockedBy) {
      toast.error(t('vram.blockedToast', { defaultValue: 'VRAM 预算超限，无法执行' }), {
        description: t('vram.blockedReason', {
          defaultValue:
            '设备 {{devices}} 超出预算，且未允许超额提交（compute.allow_overcommit=false）。请在 VRAM 账本中按建议调整：换小变体 / 改绑设备 / 停模块。',
          devices: vramBlockedBy,
        }),
        duration: 8000,
      })
      setVramOpen(true)
      return
    }
    // §5.2 MVP：变体 pin 与激活变体不一致 → 报错 + 一键切换引导（不静默热切换）
    const issues = collectPinIssues(nodes, activeModels, modelsList)
    if (issues.length > 0) {
      setPinIssues(issues)
      setPinDialogOpen(true)
      return
    }
    const issues2 = validatePipeline()
    if (issues2.length > 0) {
      toast.error(t('exec.validationFailed'), {
        description:
          issues2.slice(0, 4).join(t('validate.issueSeparator')) +
          (issues2.length > 4 ? '…' : ''),
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
  }, [executing, vramBlockedBy, validatePipeline, nodes, activeModels, modelsList, t])

  /** 执行对话框提交：合并参数 → 清空旧状态 → executePipeline（§6.5 wait/callback）→ 任务链接 */
  const handleSubmitExecution = useCallback(
    async (values: Record<string, ParamValue>, opts: ExecuteDialogOptions) => {
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
        // §6.5 无人值守选项：wait 同步模式 / callback_url 完成回调
        if (opts.wait) body.wait = true
        if (opts.callbackUrl) body.callback_url = opts.callbackUrl

        // P1 wait 同步模式同样上执行锁：请求在服务端阻塞至终态（可能数分钟），
        // 若不加锁，阻塞期间工具栏执行按钮仍可点，并发提交会同时写同一 workspace。
        // 提交完成或失败后解锁（下方 wait 分支 / catch 分支）。
        if (opts.wait) {
          executingRef.current = true
          setExecuting(true)
        }

        const resp = await api.executePipeline(body)
        setExecDialogOpen(false)

        if (opts.wait) {
          // 同步模式：请求已阻塞至终态，直接呈现结果
          executingRef.current = false
          setExecuting(false)
          activeTaskIdRef.current = null
          const status = (resp.status ?? '').trim().toLowerCase()
          if (status === 'completed') {
            toast.success(t('exec.waitDone', { defaultValue: '任务已完成' }), {
              description: t('exec.taskId', { taskId: resp.task_id }),
            })
          } else if (status === 'failed' || status === 'cancelled') {
            toast.error(
              t('exec.waitTerminal', { defaultValue: '任务终态：{{status}}', status: resp.status }),
              { description: t('exec.taskId', { taskId: resp.task_id }) },
            )
          } else {
            toast.success(t('exec.submitted'), {
              description: t('exec.taskId', { taskId: resp.task_id }),
            })
          }
          // 以任务注册表状态刷新画布节点
          try {
            const detail = await api.getTask(resp.task_id)
            const stateByNode = new Map(
              (detail.nodes ?? []).map((nr) => [nr.node_id, nr.state]),
            )
            setNodes((nds) =>
              nds.map((n) => {
                const state = stateByNode.get(n.id)
                if (state === undefined) return n
                return {
                  ...n,
                  data: { ...n.data, status: normalizeNodeStatus(state) },
                } as PipelineFlowNode
              }),
            )
          } catch {
            // 查询失败不影响提交结果展示
          }
          return
        }

        // 异步模式：上执行锁 + 记录 task_id（WS 过滤与终态轮询消费）
        activeTaskIdRef.current = resp.task_id
        pollErrorsRef.current = 0
        executingRef.current = true
        setExecuting(true)
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
        // wait 模式提交失败（超时 / 网络中断）：释放执行锁，避免永久锁死误拦重试
        if (opts.wait) {
          executingRef.current = false
          setExecuting(false)
          activeTaskIdRef.current = null
        }
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
        canExport={currentId != null}
        onExport={handleExportCurrent}
        onImportShare={(f) => void handleImportShare(f)}
        canSchedule={currentId != null}
        onSchedule={() => void openScheduleDialog()}
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
        onShowTasks={() => setTasksDialogOpen(true)}
        canExport={nodes.length > 0}
        onExport={handleExportToml}
        onImport={() => importFileRef.current?.click()}
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

        <div
          ref={canvasRef}
          className={cn('relative min-w-0 flex-1', executing && 'ep-executing')}
        >
          {/* 连线渐变定义：隐藏 SVG，供 RF_THEME_CSS 的 url(#ep-edge-gradient) 引用（纯视觉） */}
          <svg className="absolute h-0 w-0" aria-hidden="true" focusable="false">
            <defs>
              <linearGradient id="ep-edge-gradient" x1="0%" y1="0%" x2="100%" y2="0%">
                <stop offset="0%" stopColor="var(--accent-gradient-from)" />
                <stop offset="100%" stopColor="var(--accent-gradient-to)" />
              </linearGradient>
            </defs>
          </svg>
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
              <div className="glass hidden max-w-[28rem] flex-wrap items-center justify-center gap-x-3 gap-y-1 rounded-lg border border-border-glow px-3.5 py-1.5 shadow-md md:flex">
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
            {/* VRAM 账本开关：超限时红点提醒（执行阻断的可见入口） */}
            <Panel position="top-right">
              <Button
                variant={vramOpen ? 'default' : 'outline'}
                size="xs"
                onClick={() => {
                  setVramOpen((open) => !open)
                  // 打开面板时刷新一次估算（设备占用随时间变化）
                  if (!vramOpen && hasModuleNodes) fetchVramBudget()
                }}
                title={t('vram.toggleTitle', { defaultValue: 'VRAM 每设备账本（§6.3）' })}
                aria-label={t('vram.toggleTitle', { defaultValue: 'VRAM 每设备账本（§6.3）' })}
                aria-pressed={vramOpen}
                className="shadow-sm"
              >
                <MemoryStick className="h-3.5 w-3.5" />
                VRAM
                {vramBlockedBy && (
                  <span
                    className="h-1.5 w-1.5 rounded-full bg-status-error"
                    title={t('vram.over', { defaultValue: '超出预算' })}
                  />
                )}
              </Button>
            </Panel>
          </ReactFlow>

          {nodes.length === 0 && (
            <div className="pointer-events-none absolute inset-0 z-10 flex flex-col items-center justify-center gap-3">
              <span className="glass flex h-14 w-14 items-center justify-center rounded-2xl border border-dashed border-border-glow">
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

        {/* VRAM 每设备账本（§6.3 画布侧栏）：桌面常驻右栏 / 窄屏 overlay */}
        {vramOpen && (
          <VramLedgerPanel
            report={vramReport}
            loading={vramLoading}
            error={vramError}
            deviceNames={deviceNames}
            nodeLabels={nodeLabels}
            hasModuleNodes={hasModuleNodes}
            onRefresh={fetchVramBudget}
            onClose={() => setVramOpen(false)}
            className={
              isDesktop ? undefined : 'absolute inset-y-0 right-0 z-20 max-w-[85vw] shadow-lg'
            }
          />
        )}

        {/* 桌面（≥lg）：参数面板常驻右栏；窄屏：右侧 overlay */}
        {selectedNode && (
          <NodeParamsPanel
            node={selectedNode}
            onParamsChange={(patch) => updateNodeParams(selectedNode.id, patch)}
            onDelete={() => void requestDeleteNode(selectedNode.id)}
            onClose={() => setSelectedNodeId(null)}
            onBindingChange={(patch) => updateNodeBinding(selectedNode.id, patch)}
            onCapabilityChange={(capabilityId) =>
              changeNodeCapability(selectedNode.id, capabilityId)
            }
            variants={selectedModuleVariants}
            devices={devices}
            activeVariant={selectedModuleActiveVariant}
            className={
              isDesktop ? undefined : 'absolute inset-y-0 right-0 z-30 max-w-[85vw] shadow-lg'
            }
          />
        )}
      </div>

      {/* §6.4 TOML 导入文件选择（隐藏 input） */}
      <input
        ref={importFileRef}
        type="file"
        accept=".toml,text/plain"
        className="hidden"
        aria-hidden="true"
        tabIndex={-1}
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ''
          if (file) void handleImportFile(file)
        }}
      />

      <SaveAsDialog
        open={saveAsOpen}
        onOpenChange={setSaveAsOpen}
        defaultName={name}
        pipelines={pipelines}
        onConfirm={handleSaveAsConfirm}
      />

      <ScheduleDialog
        open={scheduleOpen}
        onOpenChange={setScheduleOpen}
        pipelineId={currentId}
        form={scheduleForm}
        onFormChange={setScheduleForm}
        current={scheduleCurrent}
        onSave={() => void saveSchedule()}
        onRemove={() => void removeSchedule()}
      />

      <ExecuteDialog
        open={execDialogOpen}
        onOpenChange={setExecDialogOpen}
        fields={execFields}
        submitting={execSubmitting}
        onSubmit={(values, opts) => void handleSubmitExecution(values, opts)}
      />

      <VariantPinDialog
        open={pinDialogOpen}
        onOpenChange={setPinDialogOpen}
        issues={pinIssues}
        activeModels={activeModels}
        onSwitch={switchVariantToPin}
      />

      <PipelineTasksDialog
        open={tasksDialogOpen}
        onOpenChange={setTasksDialogOpen}
        pipelineId={currentId}
      />

      <ImportTomlDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        draft={importDraft}
        pipelines={pipelines}
        onConfirm={handleImportConfirm}
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
