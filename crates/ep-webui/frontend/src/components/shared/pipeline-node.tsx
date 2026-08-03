import { Handle, Position } from '@xyflow/react'
import type { Node, NodeProps, NodeTypes } from '@xyflow/react'
import {
  AudioLines,
  Brain,
  FileInput,
  FileOutput,
  FileVideo,
  Film,
  Globe,
  Image as ImageIcon,
  Languages,
  Mic,
  Package,
  Sparkles,
  Type,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { categoryLabel } from '@/lib/constants'

// ============================================================
// 数据类型（端口）
// ============================================================

export type DataType = 'audio' | 'video' | 'image' | 'text' | 'file' | 'any'

interface DataTypeMeta {
  label: string
  /** 端口圆点配色（handle 使用 important 工具类覆盖 React Flow 默认样式） */
  handle: string
  /** 图例 / 标签用色点 */
  chip: string
}

export const DATA_TYPE_META: Record<DataType, DataTypeMeta> = {
  audio: { label: '音频', handle: 'bg-dtype-audio! border-card!', chip: 'bg-dtype-audio' },
  video: { label: '视频', handle: 'bg-dtype-video! border-card!', chip: 'bg-dtype-video' },
  image: { label: '图像', handle: 'bg-dtype-image! border-card!', chip: 'bg-dtype-image' },
  text: { label: '文本', handle: 'bg-dtype-text! border-card!', chip: 'bg-dtype-text' },
  file: { label: '文件', handle: 'bg-dtype-file! border-card!', chip: 'bg-dtype-file' },
  any: { label: '任意', handle: 'bg-dtype-any! border-card!', chip: 'bg-dtype-any' },
}

/** 端口数据类型兼容规则：相同、任一为 any、或任一为 file（文件可承载任何媒体类型） */
export function dataTypesCompatible(a: DataType, b: DataType): boolean {
  return a === b || a === 'any' || b === 'any' || a === 'file' || b === 'file'
}

export interface Port {
  id: string
  label: string
  dataType: DataType
}

// ============================================================
// 节点运行状态
// ============================================================

export type NodeStatus = 'waiting' | 'running' | 'done' | 'failed'

interface NodeStatusMeta {
  label: string
  /** 节点卡片边框色 */
  border: string
  /** 状态圆点（运行中带脉冲动画） */
  dot: string
  /** 状态光晕 */
  glow: string
}

export const NODE_STATUS_META: Record<NodeStatus, NodeStatusMeta> = {
  waiting: { label: '等待', border: 'border-border', dot: 'bg-status-stopped', glow: '' },
  running: {
    label: '运行中',
    border: 'border-status-starting',
    dot: 'bg-status-starting animate-pulse',
    glow: 'shadow-md shadow-status-starting/30',
  },
  done: {
    label: '完成',
    border: 'border-status-running',
    dot: 'bg-status-running',
    glow: 'shadow-md shadow-status-running/25',
  },
  failed: {
    label: '失败',
    border: 'border-status-error',
    dot: 'bg-status-error',
    glow: 'shadow-md shadow-status-error/30',
  },
}

/** 将后端推送的状态字符串归一化为节点状态 */
export function normalizeNodeStatus(status: string | null | undefined): NodeStatus {
  switch ((status ?? '').trim().toLowerCase()) {
    case 'running':
    case 'starting':
    case 'processing':
      return 'running'
    case 'done':
    case 'success':
    case 'succeeded':
    case 'completed':
    case 'finished':
      return 'done'
    case 'failed':
    case 'error':
    case 'errored':
      return 'failed'
    default:
      return 'waiting'
  }
}

// ============================================================
// 参数表单（由能力参数模式自动生成）
// ============================================================

export type ParamValue = string | number | boolean
export type NodeParams = Record<string, ParamValue>

export interface ParamSpec {
  name: string
  label: string
  type: 'string' | 'number' | 'boolean' | 'select'
  options?: string[]
  defaultValue?: ParamValue
  placeholder?: string
  hint?: string
  required?: boolean
}

export function defaultParams(specs: ParamSpec[]): NodeParams {
  const out: NodeParams = {}
  for (const spec of specs) {
    if (spec.defaultValue !== undefined) out[spec.name] = spec.defaultValue
  }
  return out
}

// ============================================================
// 内置节点定义
// ============================================================

export type BuiltinKind = 'file_input' | 'file_output' | 'ffmpeg'

export interface BuiltinDef {
  kind: BuiltinKind
  label: string
  description: string
  icon: LucideIcon
  accent: string
  inputs: Port[]
  outputs: Port[]
  params: ParamSpec[]
}

export const BUILTIN_DEFS: Record<BuiltinKind, BuiltinDef> = {
  file_input: {
    kind: 'file_input',
    label: '文件输入',
    description: '从磁盘读取输入文件',
    icon: FileInput,
    accent: 'bg-node-file-input/15 text-node-file-input',
    inputs: [],
    outputs: [{ id: 'out', label: '输出', dataType: 'file' }],
    params: [
      { name: 'path', label: '文件路径', type: 'string', required: true, placeholder: '/workspace/input/audio.wav' },
      { name: 'pattern', label: '匹配模式', type: 'string', placeholder: '*.wav（批量时生效）' },
    ],
  },
  file_output: {
    kind: 'file_output',
    label: '文件输出',
    description: '将结果写入磁盘文件',
    icon: FileOutput,
    accent: 'bg-node-file-output/15 text-node-file-output',
    inputs: [{ id: 'in', label: '输入', dataType: 'file' }],
    outputs: [],
    params: [
      { name: 'path', label: '输出路径', type: 'string', required: true, placeholder: '/workspace/output/result.txt' },
      { name: 'overwrite', label: '覆盖已有文件', type: 'boolean', defaultValue: true },
    ],
  },
  ffmpeg: {
    kind: 'ffmpeg',
    label: 'FFmpeg 处理',
    description: '音视频转码 / 剪辑 / 混流',
    icon: Film,
    accent: 'bg-node-ffmpeg/15 text-node-ffmpeg',
    inputs: [{ id: 'in', label: '输入', dataType: 'file' }],
    outputs: [{ id: 'out', label: '输出', dataType: 'file' }],
    params: [
      {
        name: 'args',
        label: '命令参数',
        type: 'string',
        placeholder: '-i {input} -c:v libx264 {output}',
        hint: '{input}/{output} 为占位符',
      },
      { name: 'timeout_secs', label: '超时（秒）', type: 'number', defaultValue: 600 },
    ],
  },
}

export const BUILTIN_LIST: BuiltinDef[] = [
  BUILTIN_DEFS.file_input,
  BUILTIN_DEFS.file_output,
  BUILTIN_DEFS.ffmpeg,
]

// ============================================================
// 外部 API 节点
// ============================================================

export const EXTERNAL_PARAMS: ParamSpec[] = [
  { name: 'endpoint', label: '接口地址', type: 'string', required: true, placeholder: 'https://api.example.com/v1/process' },
  { name: 'method', label: '请求方法', type: 'select', options: ['GET', 'POST', 'PUT'], defaultValue: 'POST' },
  { name: 'api_key', label: 'API Key', type: 'string', placeholder: 'sk-…（可选）' },
  { name: 'timeout_secs', label: '超时（秒）', type: 'number', defaultValue: 60 },
]

// ============================================================
// 模块能力（按分类推导输入输出与参数模式）
// ============================================================

export interface CapabilityDef {
  id: string
  label: string
  inputs: Port[]
  outputs: Port[]
  params: ParamSpec[]
}

const DEVICE_PARAM: ParamSpec = {
  name: 'device',
  label: '计算设备',
  type: 'select',
  options: ['auto', 'cuda', 'cpu'],
  defaultValue: 'auto',
}

export function moduleCapability(category: string): CapabilityDef {
  const io = (input: DataType, output: DataType): Pick<CapabilityDef, 'inputs' | 'outputs'> => ({
    inputs: [{ id: 'in', label: '输入', dataType: input }],
    outputs: [{ id: 'out', label: '输出', dataType: output }],
  })

  switch (category.toLowerCase()) {
    case 'asr':
      return {
        id: 'asr.transcribe',
        label: '语音转写',
        ...io('audio', 'text'),
        params: [
          { name: 'language', label: '语言', type: 'select', options: ['zh', 'en', 'ja', 'yue'], defaultValue: 'zh' },
          { name: 'model', label: '模型', type: 'string', defaultValue: 'paraformer-v2', hint: '模型 ID' },
          DEVICE_PARAM,
        ],
      }
    case 'tts':
      return {
        id: 'tts.synthesize',
        label: '语音合成',
        ...io('text', 'audio'),
        params: [
          { name: 'voice', label: '音色', type: 'select', options: ['xiaoyun', 'xiaoxiao', 'alex'], defaultValue: 'xiaoyun' },
          { name: 'speed', label: '语速', type: 'number', defaultValue: 1.0, hint: '0.5 – 2.0' },
          { name: 'format', label: '输出格式', type: 'select', options: ['wav', 'mp3', 'flac'], defaultValue: 'wav' },
        ],
      }
    case 'denoise':
      return {
        id: 'denoise.enhance',
        label: '音频降噪',
        ...io('audio', 'audio'),
        params: [
          { name: 'strength', label: '降噪强度', type: 'number', defaultValue: 0.7, hint: '0 – 1' },
          { name: 'model', label: '模型', type: 'string', defaultValue: 'deepfilternet3' },
        ],
      }
    case 'ocr':
      return {
        id: 'ocr.recognize',
        label: '文字识别',
        ...io('image', 'text'),
        params: [
          { name: 'language', label: '语言', type: 'select', options: ['zh', 'en', 'multi'], defaultValue: 'zh' },
          { name: 'threshold', label: '置信度阈值', type: 'number', defaultValue: 0.5, hint: '0 – 1' },
        ],
      }
    case 'image':
      return {
        id: 'image.process',
        label: '图像处理',
        ...io('image', 'image'),
        params: [
          { name: 'operation', label: '操作', type: 'select', options: ['resize', 'crop', 'rotate', 'watermark'], defaultValue: 'resize' },
          { name: 'quality', label: '输出质量', type: 'number', defaultValue: 90, hint: '1 – 100' },
        ],
      }
    case 'video':
      return {
        id: 'video.transcode',
        label: '视频转码',
        ...io('video', 'video'),
        params: [
          { name: 'codec', label: '编码器', type: 'select', options: ['libx264', 'libx265', 'copy'], defaultValue: 'libx264' },
          { name: 'fps', label: '帧率', type: 'number', defaultValue: 30 },
          { name: 'resolution', label: '分辨率', type: 'string', placeholder: '1920x1080（留空保持原始）' },
        ],
      }
    case 'audio':
      return {
        id: 'audio.convert',
        label: '音频转换',
        ...io('audio', 'audio'),
        params: [
          { name: 'sample_rate', label: '采样率', type: 'select', options: ['8000', '16000', '44100', '48000'], defaultValue: '16000' },
          { name: 'channels', label: '声道', type: 'select', options: ['1', '2'], defaultValue: '1' },
        ],
      }
    case 'translate':
      return {
        id: 'translate.translate',
        label: '机器翻译',
        ...io('text', 'text'),
        params: [
          { name: 'source_lang', label: '源语言', type: 'select', options: ['auto', 'zh', 'en', 'ja'], defaultValue: 'auto' },
          { name: 'target_lang', label: '目标语言', type: 'select', options: ['zh', 'en', 'ja'], defaultValue: 'en' },
          { name: 'model', label: '模型', type: 'string', defaultValue: 'nllb-200' },
        ],
      }
    case 'llm':
      return {
        id: 'llm.generate',
        label: '文本生成',
        ...io('text', 'text'),
        params: [
          { name: 'model', label: '模型', type: 'string', defaultValue: 'qwen2.5-7b-instruct' },
          { name: 'temperature', label: 'Temperature', type: 'number', defaultValue: 0.7, hint: '0 – 2' },
          { name: 'max_tokens', label: '最大 Token 数', type: 'number', defaultValue: 2048 },
        ],
      }
    default:
      return {
        id: 'other.process',
        label: '通用处理',
        ...io('file', 'file'),
        params: [
          { name: 'input_path', label: '输入路径', type: 'string', placeholder: '/workspace/input' },
          { name: 'output_path', label: '输出路径', type: 'string', placeholder: '/workspace/output' },
          { name: 'extra_args', label: '附加参数', type: 'string', placeholder: '--flag value' },
        ],
      }
  }
}

// ============================================================
// 分类视觉（图标 + 强调色）
// ============================================================

const CATEGORY_ICONS: Record<string, LucideIcon> = {
  asr: Mic,
  tts: AudioLines,
  denoise: Sparkles,
  ocr: Type,
  image: ImageIcon,
  video: FileVideo,
  audio: AudioLines,
  translate: Languages,
  llm: Brain,
  other: Package,
}

const CATEGORY_ACCENTS: Record<string, string> = {
  asr: 'bg-cat-asr/15 text-cat-asr',
  tts: 'bg-cat-tts/15 text-cat-tts',
  denoise: 'bg-cat-denoise/15 text-cat-denoise',
  ocr: 'bg-cat-ocr/15 text-cat-ocr',
  image: 'bg-cat-image/15 text-cat-image',
  video: 'bg-cat-video/15 text-cat-video',
  audio: 'bg-cat-audio/15 text-cat-audio',
  translate: 'bg-cat-translate/15 text-cat-translate',
  llm: 'bg-cat-llm/15 text-cat-llm',
  other: 'bg-cat-other/15 text-cat-other',
}

export function categoryVisual(category: string): { icon: LucideIcon; accent: string } {
  const key = category.toLowerCase()
  return {
    icon: CATEGORY_ICONS[key] ?? Package,
    accent: CATEGORY_ACCENTS[key] ?? CATEGORY_ACCENTS.other,
  }
}

// ============================================================
// 节点数据模型
// ============================================================

type NodeDataBase = {
  label: string
  status: NodeStatus
  params: NodeParams
}

export type ModuleNodeData = NodeDataBase & {
  kind: 'module'
  moduleId: string
  moduleVersion: string
  category: string
  capabilityId: string
  capabilityLabel: string
}

export type BuiltinNodeData = NodeDataBase & {
  kind: 'builtin'
  builtin: BuiltinKind
}

export type ExternalNodeData = NodeDataBase & {
  kind: 'external'
  endpoint: string
  method: 'GET' | 'POST' | 'PUT'
}

export type PipelineNodeData = ModuleNodeData | BuiltinNodeData | ExternalNodeData

export type ModuleFlowNode = Node<ModuleNodeData, 'module'>
export type BuiltinFlowNode = Node<BuiltinNodeData, 'builtin'>
export type ExternalFlowNode = Node<ExternalNodeData, 'external'>
export type PipelineFlowNode = ModuleFlowNode | BuiltinFlowNode | ExternalFlowNode

export function getNodePorts(data: PipelineNodeData): { inputs: Port[]; outputs: Port[] } {
  switch (data.kind) {
    case 'module': {
      const cap = moduleCapability(data.category)
      return { inputs: cap.inputs, outputs: cap.outputs }
    }
    case 'builtin': {
      const def = BUILTIN_DEFS[data.builtin]
      return { inputs: def.inputs, outputs: def.outputs }
    }
    case 'external':
      return {
        inputs: [{ id: 'in', label: '输入', dataType: 'any' }],
        outputs: [{ id: 'out', label: '输出', dataType: 'any' }],
      }
  }
}

export function getParamSpecs(data: PipelineNodeData): ParamSpec[] {
  switch (data.kind) {
    case 'module':
      return moduleCapability(data.category).params
    case 'builtin':
      return BUILTIN_DEFS[data.builtin].params
    case 'external':
      return EXTERNAL_PARAMS
  }
}

export function nodeKindLabel(data: PipelineNodeData): string {
  switch (data.kind) {
    case 'module':
      return `模块节点 · ${data.capabilityLabel}`
    case 'builtin':
      return `内置节点 · ${BUILTIN_DEFS[data.builtin].label}`
    case 'external':
      return `外部 API · ${data.method}`
  }
}

// ============================================================
// 拖拽载荷与管线序列化格式
// ============================================================

export const DRAG_MIME = 'application/reactflow'

export interface DragPayload {
  nodeType: 'module' | 'builtin' | 'external'
  moduleId?: string
  moduleName?: string
  moduleVersion?: string
  category?: string
  builtin?: BuiltinKind
}

export interface PipelineDefinition {
  name: string
  version: number
  nodes: {
    id: string
    type: string
    position: { x: number; y: number }
    data: PipelineNodeData
  }[]
  edges: {
    id: string
    source: string
    target: string
    sourceHandle?: string | null
    targetHandle?: string | null
  }[]
}

function uid(prefix: string): string {
  return `${prefix}-${crypto.randomUUID().slice(0, 8)}`
}

/** 由拖拽载荷创建节点（含默认参数快照） */
export function createPipelineNode(
  payload: DragPayload,
  position: { x: number; y: number },
): PipelineFlowNode {
  if (payload.nodeType === 'module') {
    const category = payload.category ?? 'other'
    const cap = moduleCapability(category)
    const data: ModuleNodeData = {
      kind: 'module',
      label: payload.moduleName ?? payload.moduleId ?? '未命名模块',
      moduleId: payload.moduleId ?? 'unknown',
      moduleVersion: payload.moduleVersion ?? '0.1.0',
      category,
      capabilityId: cap.id,
      capabilityLabel: cap.label,
      status: 'waiting',
      params: defaultParams(cap.params),
    }
    return { id: uid('module'), type: 'module', position, data }
  }

  if (payload.nodeType === 'builtin' && payload.builtin) {
    const def = BUILTIN_DEFS[payload.builtin]
    const data: BuiltinNodeData = {
      kind: 'builtin',
      builtin: payload.builtin,
      label: def.label,
      status: 'waiting',
      params: defaultParams(def.params),
    }
    return { id: uid('builtin'), type: 'builtin', position, data }
  }

  const data: ExternalNodeData = {
    kind: 'external',
    label: '外部 API',
    endpoint: 'https://api.example.com/v1/process',
    method: 'POST',
    status: 'waiting',
    params: defaultParams(EXTERNAL_PARAMS),
  }
  return { id: uid('external'), type: 'external', position, data }
}

// ============================================================
// 节点卡片组件
// ============================================================

function NodeCard({
  status,
  selected,
  children,
}: {
  status: NodeStatus
  selected?: boolean
  children: React.ReactNode
}) {
  const meta = NODE_STATUS_META[status]
  return (
    <div
      className={cn(
        'w-56 rounded-lg border-2 bg-card text-card-foreground transition-all duration-200',
        meta.border,
        meta.glow,
        selected
          ? 'shadow-xl ring-2 ring-ring/70'
          : 'shadow-sm hover:-translate-y-px hover:shadow-lg',
      )}
    >
      {children}
    </div>
  )
}

function StatusDot({ status }: { status: NodeStatus }) {
  const meta = NODE_STATUS_META[status]
  return (
    <span
      title={`状态：${meta.label}`}
      className={cn('h-2 w-2 shrink-0 rounded-full transition-colors', meta.dot)}
    />
  )
}

/** 端口行：handle 锚定在本行（relative），左侧输入 / 右侧输出，标签按数据类型着色 */
function PortRow({ inputs, outputs }: { inputs: Port[]; outputs: Port[] }) {
  return (
    <div className="relative flex h-7 items-center justify-between rounded-b-md border-t border-border/70 bg-muted/30 px-2.5">
      <span className="flex items-center">
        {inputs.map((port) => (
          <PortLabel key={port.id} port={port} side="in" />
        ))}
      </span>
      <span className="flex items-center">
        {outputs.map((port) => (
          <PortLabel key={port.id} port={port} side="out" />
        ))}
      </span>
    </div>
  )
}

function PortLabel({ port, side }: { port: Port; side: 'in' | 'out' }) {
  const meta = DATA_TYPE_META[port.dataType]
  return (
    <>
      <Handle
        type={side === 'in' ? 'target' : 'source'}
        position={side === 'in' ? Position.Left : Position.Right}
        id={port.id}
        title={`${meta.label}端口`}
        className={cn(
          'h-3! w-3! rounded-full! border-2! transition-transform!',
          'hover:scale-125!',
          meta.handle,
        )}
      />
      <span
        className={cn(
          'flex items-center gap-1 text-[10px] text-muted-foreground',
          side === 'in' ? 'pl-3' : 'pr-3',
        )}
      >
        <span className={cn('h-1.5 w-1.5 rounded-full', meta.chip)} />
        {meta.label}
      </span>
    </>
  )
}

/** 模块节点：模块名 + 能力 + 分类，状态色边框 */
export function ModuleNode({ data, selected }: NodeProps<ModuleFlowNode>) {
  const cap = moduleCapability(data.category)
  const visual = categoryVisual(data.category)
  const Icon = visual.icon
  return (
    <NodeCard status={data.status} selected={selected}>
      <div className="flex items-center gap-2 px-3 pb-2 pt-2.5">
        <span
          className={cn(
            'flex h-7 w-7 shrink-0 items-center justify-center rounded-md transition-transform duration-200 group-hover:scale-105',
            visual.accent,
          )}
        >
          <Icon className="h-4 w-4" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-semibold leading-tight">{data.label}</p>
          <p className="truncate text-[10px] text-muted-foreground">
            {cap.label} · {categoryLabel(data.category)}
          </p>
        </div>
        <StatusDot status={data.status} />
      </div>
      <div className="truncate border-t border-border/70 px-3 py-1 font-mono text-[10px] text-muted-foreground">
        {data.moduleId}@{data.moduleVersion}
      </div>
      <PortRow inputs={cap.inputs} outputs={cap.outputs} />
    </NodeCard>
  )
}

/** 内置节点：file_input / file_output / ffmpeg */
export function BuiltinNode({ data, selected }: NodeProps<BuiltinFlowNode>) {
  const def = BUILTIN_DEFS[data.builtin]
  const Icon = def.icon
  const preview =
    data.builtin === 'ffmpeg'
      ? typeof data.params.args === 'string' && data.params.args
        ? data.params.args
        : null
      : typeof data.params.path === 'string' && data.params.path
        ? data.params.path
        : null
  return (
    <NodeCard status={data.status} selected={selected}>
      <div className="flex items-center gap-2 px-3 pb-2 pt-2.5">
        <span
          className={cn(
            'flex h-7 w-7 shrink-0 items-center justify-center rounded-md',
            def.accent,
          )}
        >
          <Icon className="h-4 w-4" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-semibold leading-tight">{def.label}</p>
          <p className="truncate text-[10px] text-muted-foreground">{def.description}</p>
        </div>
        <span className="shrink-0 rounded bg-muted px-1 py-px text-[9px] text-muted-foreground">
          内置
        </span>
        <StatusDot status={data.status} />
      </div>
      {preview && (
        <div className="truncate border-t border-border/70 px-3 py-1 font-mono text-[10px] text-muted-foreground">
          {preview}
        </div>
      )}
      <PortRow inputs={def.inputs} outputs={def.outputs} />
    </NodeCard>
  )
}

const METHOD_BADGES: Record<ExternalNodeData['method'], string> = {
  GET: 'bg-http-get/15 text-http-get',
  POST: 'bg-http-post/15 text-http-post',
  PUT: 'bg-http-put/15 text-http-put',
}

/** 外部 API 节点：展示接口地址与请求方法 */
export function ExternalApiNode({ data, selected }: NodeProps<ExternalFlowNode>) {
  const endpoint =
    typeof data.params.endpoint === 'string' && data.params.endpoint
      ? data.params.endpoint
      : data.endpoint
  return (
    <NodeCard status={data.status} selected={selected}>
      <div className="flex items-center gap-2 px-3 pb-2 pt-2.5">
        <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-node-external/15 text-node-external">
          <Globe className="h-4 w-4" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-[13px] font-semibold leading-tight">{data.label}</p>
          <p className="truncate text-[10px] text-muted-foreground">调用外部 HTTP 接口</p>
        </div>
        <span
          className={cn(
            'shrink-0 rounded px-1 py-px font-mono text-[9px] font-semibold',
            METHOD_BADGES[data.method],
          )}
        >
          {data.method}
        </span>
        <StatusDot status={data.status} />
      </div>
      <div className="truncate border-t border-border/70 px-3 py-1 font-mono text-[10px] text-muted-foreground">
        {endpoint}
      </div>
      <PortRow
        inputs={[{ id: 'in', label: '输入', dataType: 'any' }]}
        outputs={[{ id: 'out', label: '输出', dataType: 'any' }]}
      />
    </NodeCard>
  )
}

export const pipelineNodeTypes: NodeTypes = {
  module: ModuleNode,
  builtin: BuiltinNode,
  external: ExternalApiNode,
}
