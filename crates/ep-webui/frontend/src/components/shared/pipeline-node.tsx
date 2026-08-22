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
  Plus,
  Sparkles,
  Trash2,
  Type,
  type LucideIcon,
} from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'
import { categoryLabel } from '@/lib/constants'
import i18n from '@/i18n'
import type { CapabilityDecl, CapabilityParamSchema, DeviceResponse } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'

/**
 * 模块级翻译助手：静态元数据（常量/工厂函数）无法使用 React Hook，
 * 在读取时按当前语言即时解析，保证语言切换后重渲染即可生效。
 * 键位于 components 命名空间，跨命名空间用 "common:xxx" 全限定键。
 * 新增键统一携带 defaultValue 兜底（键集由 C8 统一落盘）。
 */
function t(key: string, options?: Record<string, unknown>): string {
  return i18n.t(key, options) as string
}

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
  audio: { get label() { return t('components:pipeline.dataType.audio') }, handle: 'bg-dtype-audio! border-card!', chip: 'bg-dtype-audio' },
  video: { get label() { return t('components:pipeline.dataType.video') }, handle: 'bg-dtype-video! border-card!', chip: 'bg-dtype-video' },
  image: { get label() { return t('components:pipeline.dataType.image') }, handle: 'bg-dtype-image! border-card!', chip: 'bg-dtype-image' },
  text: { get label() { return t('components:pipeline.dataType.text') }, handle: 'bg-dtype-text! border-card!', chip: 'bg-dtype-text' },
  file: { get label() { return t('components:pipeline.dataType.file') }, handle: 'bg-dtype-file! border-card!', chip: 'bg-dtype-file' },
  any: { get label() { return t('components:pipeline.dataType.any') }, handle: 'bg-dtype-any! border-card!', chip: 'bg-dtype-any' },
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

/**
 * 后端能力 input_type/output_type（manifest DataType，audio/video/image/
 * text/json/file…）→ 端口数据类型（null 安全；未知类型回退 any）。
 * json 归一为 text：结构化文本（如 ASR 分段 JSON）可被文本消费方直接接收。
 */
export function normalizeDataType(raw: string | null | undefined): DataType {
  switch ((raw ?? '').trim().toLowerCase()) {
    case 'audio':
      return 'audio'
    case 'video':
      return 'video'
    case 'image':
      return 'image'
    case 'text':
    case 'json':
      return 'text'
    case 'file':
      return 'file'
    default:
      return 'any'
  }
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
  waiting: {
    get label() { return t('components:pipeline.nodeStatus.waiting') },
    border: 'border-border-glow!',
    dot: 'bg-status-stopped',
    glow: '',
  },
  running: {
    get label() { return t('components:pipeline.nodeStatus.running') },
    border: 'border-status-running!',
    dot: 'bg-status-running animate-pulse shadow-[0_0_8px_var(--status-glow-running)]',
    // 强档辉光（12px+）仅限运行中节点（§3.1 规则 2）
    glow: 'glow-status-running',
  },
  done: {
    get label() { return t('components:pipeline.nodeStatus.done') },
    border: 'border-status-running/60!',
    dot: 'bg-status-running',
    glow: 'glow-status-done',
  },
  failed: {
    get label() { return t('components:pipeline.nodeStatus.failed') },
    border: 'border-status-error!',
    dot: 'bg-status-error',
    glow: 'glow-status-failed',
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
// 参数表单类型
// ============================================================

/** 参数值；string[] 为 ffmpeg args 数组化编辑形状（P0-2，序列化恒数组） */
export type ParamValue = string | number | boolean | string[]
export type NodeParams = Record<string, ParamValue>

export interface ParamSpec {
  name: string
  label: string
  /**
   * 字段类型。textarea = 多行文本（llm.system_prompt）；
   * string_array = 逐条增删改的字符串数组（ffmpeg.args，P0-2）。
   * 渲染由 ParamSpecField 统一承接（C3 NodeParamsPanel 可整体委托）。
   */
  type: 'string' | 'number' | 'boolean' | 'select' | 'textarea' | 'string_array'
  options?: string[]
  defaultValue?: ParamValue
  placeholder?: string
  hint?: string
  required?: boolean
  /** 数值约束（manifest schema min/max/step 透传，渲染与校验消费） */
  min?: number
  max?: number
  step?: number
}

export function defaultParams(specs: ParamSpec[]): NodeParams {
  const out: NodeParams = {}
  for (const spec of specs) {
    if (spec.defaultValue === undefined) continue
    // 数组默认值拷贝一份，避免多个节点共享同一引用被串改
    out[spec.name] = Array.isArray(spec.defaultValue)
      ? [...spec.defaultValue]
      : spec.defaultValue
  }
  return out
}

// ============================================================
// 能力数据驱动（P0-1 收口：能力来自 ModuleResponse.capabilities，裸名契约）
// ============================================================

export interface CapabilityDef {
  /**
   * 能力**裸名**（§6.2 契约：executor 拼 `/predict/{capability}`，
   * adapter 只认裸名，如 `transcribe`；不含分类前缀）。
   */
  id: string
  label: string
  /** manifest 原样描述（展示用，可空） */
  description?: string
  inputs: Port[]
  outputs: Port[]
  /** 参数表单（manifest params schema 数据驱动渲染） */
  params: ParamSpec[]
}

function isFiniteNumber(v: unknown): v is number {
  return typeof v === 'number' && Number.isFinite(v)
}

/**
 * manifest 单条参数 schema → ParamSpec（null 安全）。
 * type/default/min/max/step/enum/options 均按存在性消费；未知 type 回退 string。
 */
export function capabilityParamSpec(
  name: string,
  schema: CapabilityParamSchema | null | undefined,
): ParamSpec {
  const s: Partial<CapabilityParamSchema> = schema ?? {}
  const options = (s.enum ?? s.options ?? []).filter(
    (o): o is string => typeof o === 'string' && o.length > 0,
  )
  const rawType = (typeof s.type === 'string' ? s.type : '').trim().toLowerCase()

  let type: ParamSpec['type']
  if (options.length > 0 || rawType === 'enum' || rawType === 'select') {
    type = 'select'
  } else if (['int', 'integer', 'float', 'double', 'number'].includes(rawType)) {
    type = 'number'
  } else if (['bool', 'boolean'].includes(rawType)) {
    type = 'boolean'
  } else {
    type = 'string'
  }

  const def = s.default
  let defaultValue: ParamValue | undefined =
    typeof def === 'string' || typeof def === 'boolean' || isFiniteNumber(def)
      ? def
      : undefined
  // select 默认值不在可选项内时丢弃（避免渲染出悬空选中项）
  if (type === 'select' && typeof defaultValue === 'string' && !options.includes(defaultValue)) {
    defaultValue = undefined
  }

  const hint = typeof s.description === 'string' && s.description.trim() ? s.description : undefined

  return {
    name,
    label: name,
    type,
    ...(options.length > 0 ? { options } : {}),
    ...(defaultValue !== undefined ? { defaultValue } : {}),
    ...(hint ? { hint } : {}),
    ...(isFiniteNumber(s.min) ? { min: s.min } : {}),
    ...(isFiniteNumber(s.max) ? { max: s.max } : {}),
    ...(isFiniteNumber(s.step) ? { step: s.step } : {}),
  }
}

/** capability 参数表（键 = 参数名）→ ParamSpec 列表（null 安全，跳过空条目） */
export function capabilityParamsFromSchema(
  params: Record<string, CapabilityParamSchema> | null | undefined,
): ParamSpec[] {
  if (!params) return []
  return Object.entries(params)
    .filter(([, schema]) => schema != null)
    .map(([name, schema]) => capabilityParamSpec(name, schema))
}

/**
 * CapabilityDecl（ep-core manifest 原样序列化）→ CapabilityDef。
 * 裸名、输入输出类型与参数 schema 全部来自 manifest（修 P0-1 猜测式映射）。
 * decl 无效（null / 空名）返回 null。
 */
export function capabilityFromDecl(decl: CapabilityDecl | null | undefined): CapabilityDef | null {
  if (!decl) return null
  const id = (typeof decl.name === 'string' ? decl.name : '').trim()
  if (!id) return null
  return {
    id,
    label: id,
    description: typeof decl.description === 'string' ? decl.description : '',
    inputs: [
      { id: 'in', label: t('components:pipeline.port.input'), dataType: normalizeDataType(decl.input_type) },
    ],
    outputs: [
      { id: 'out', label: t('components:pipeline.port.output'), dataType: normalizeDataType(decl.output_type) },
    ],
    params: capabilityParamsFromSchema(decl.params),
  }
}

/**
 * ModuleResponse.capabilities → CapabilityDef[]（null 安全）。
 * B5 过渡期 / 无能力模块：返回空数组，消费方展示兜底空态（不崩）。
 */
export function capabilitiesFromModule(
  caps: CapabilityDecl[] | null | undefined,
): CapabilityDef[] {
  if (!Array.isArray(caps)) return []
  const out: CapabilityDef[] = []
  const seen = new Set<string>()
  for (const decl of caps) {
    const def = capabilityFromDecl(decl)
    if (def && !seen.has(def.id)) {
      seen.add(def.id)
      out.push(def)
    }
  }
  return out
}

/**
 * 模块节点当前选中的能力。
 * - 未声明任何能力（capabilities 缺失/空）→ null（消费方展示兜底空态）；
 * - capabilityId 未命中（陈旧数据 / 未选择）→ 回退第一项，保证渲染与校验可用。
 */
export function selectedCapability(data: ModuleNodeData): CapabilityDef | null {
  const caps = data.capabilities ?? []
  if (caps.length === 0) return null
  return caps.find((c) => c.id === data.capabilityId) ?? caps[0] ?? null
}

/**
 * @deprecated P0-1 数据驱动迁移兼容垫片：硬编码「分类 → capability」映射已删除，
 * 模块能力一律来自 ModuleResponse.capabilities（capabilitiesFromModule /
 * capabilityFromDecl，裸名）。本函数仅为旧调用点（pipeline.tsx 的
 * examplePipeline / fromSpec，C3 将接线数据驱动）提供通用兜底能力，
 * 不再携带任何猜测的参数表。
 */
export function moduleCapability(_category: string): CapabilityDef {
  return {
    id: 'process',
    get label() { return t('components:pipeline.capability.other') },
    inputs: [
      { id: 'in', get label() { return t('components:pipeline.port.input') }, dataType: 'file' },
    ],
    outputs: [
      { id: 'out', get label() { return t('components:pipeline.port.output') }, dataType: 'file' },
    ],
    params: [],
  }
}

// ============================================================
// 内置节点定义
// ============================================================

export type BuiltinKind = 'file_input' | 'file_output' | 'ffmpeg' | 'llm'

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
    get label() { return t('components:pipeline.builtin.fileInput.label') },
    get description() { return t('components:pipeline.builtin.fileInput.description') },
    icon: FileInput,
    accent: 'bg-node-file-input/15 text-node-file-input',
    inputs: [],
    outputs: [{ id: 'out', get label() { return t('components:pipeline.port.output') }, dataType: 'file' }],
    // §6.1：`pattern` 后端不读，已从 UI 移除（参数面保持最小）
    params: [
      { name: 'path', get label() { return t('components:pipeline.param.filePath') }, type: 'string', required: true, placeholder: '/workspace/input/audio.wav' },
    ],
  },
  file_output: {
    kind: 'file_output',
    get label() { return t('components:pipeline.builtin.fileOutput.label') },
    get description() { return t('components:pipeline.builtin.fileOutput.description') },
    icon: FileOutput,
    accent: 'bg-node-file-output/15 text-node-file-output',
    inputs: [{ id: 'in', get label() { return t('components:pipeline.port.input') }, dataType: 'file' }],
    outputs: [],
    // §6.1：`overwrite` 后端不读（ffmpeg 恒 -y 覆盖），已从 UI 移除
    params: [
      { name: 'path', get label() { return t('components:pipeline.param.outputPath') }, type: 'string', required: true, placeholder: '/workspace/output/result.txt' },
    ],
  },
  ffmpeg: {
    kind: 'ffmpeg',
    get label() { return t('components:pipeline.builtin.ffmpeg.label') },
    get description() { return t('components:pipeline.builtin.ffmpeg.description') },
    icon: Film,
    accent: 'bg-node-ffmpeg/15 text-node-ffmpeg',
    inputs: [{ id: 'in', get label() { return t('components:pipeline.port.input') }, dataType: 'file' }],
    outputs: [{ id: 'out', get label() { return t('components:pipeline.port.output') }, dataType: 'file' }],
    params: [
      {
        // P0-2：args 数组化编辑（逐条增删改），序列化恒数组（后端契约形状）
        name: 'args',
        get label() { return t('components:pipeline.param.args') },
        type: 'string_array',
        defaultValue: ['-i', '{input}', '{output}'],
        get placeholder() { return t('components:pipeline.param.args.itemPlaceholder', { defaultValue: '单个参数，如 -c:v libx264' }) },
        get hint() { return t('components:pipeline.param.args.hintArray', { defaultValue: '每行一个参数（数组）；{input}/{output} 为占位符' }) },
      },
      {
        // P0-2：补 output_extension（决定本节点中间产物扩展名，executor 消费）
        name: 'output_extension',
        get label() { return t('components:pipeline.param.outputExtension', { defaultValue: '输出扩展名 (output_extension)' }) },
        type: 'string',
        placeholder: 'mp4 / wav / srt',
        get hint() { return t('components:pipeline.param.outputExtension.hint', { defaultValue: '本节点输出产物的扩展名' }) },
      },
    ],
  },
  llm: {
    // §6.7：OpenAI 兼容 LLM builtin（chat/completions 单一形状）。
    // 规范 TOML 形状 = kind="builtin" + builtin="llm"；external_api 不进 palette。
    kind: 'llm',
    get label() { return t('components:pipeline.builtin.llm.label', { defaultValue: 'LLM（OpenAI 兼容）' }) },
    get description() { return t('components:pipeline.builtin.llm.description', { defaultValue: '翻译 / 摘要 / 润色（chat/completions）' }) },
    icon: Brain,
    accent: 'bg-cat-llm/15 text-cat-llm',
    inputs: [{ id: 'in', get label() { return t('components:pipeline.port.input') }, dataType: 'text' }],
    outputs: [{ id: 'out', get label() { return t('components:pipeline.port.output') }, dataType: 'text' }],
    params: [
      {
        name: 'base_url',
        get label() { return t('components:pipeline.param.baseUrl', { defaultValue: '接口地址 (base_url)' }) },
        type: 'string',
        required: true,
        placeholder: 'https://api.openai.com/v1',
      },
      {
        name: 'model',
        get label() { return t('components:pipeline.param.llmModel', { defaultValue: '模型名称 (model)' }) },
        type: 'string',
        required: true,
        placeholder: 'gpt-4o-mini / qwen-plus',
      },
      {
        // 存环境变量名而非明文密钥：执行时由后端读取环境变量，绝不收集/落盘密钥
        name: 'api_key_env',
        get label() { return t('components:pipeline.param.apiKeyEnv', { defaultValue: 'API Key 环境变量名' }) },
        type: 'string',
        placeholder: 'OPENAI_API_KEY',
        get hint() { return t('components:pipeline.param.apiKeyEnv.hint', { defaultValue: '只填环境变量名，切勿填写密钥本身；执行时从环境读取，绝不收集或落盘明文密钥' }) },
      },
      {
        name: 'system_prompt',
        get label() { return t('components:pipeline.param.systemPrompt', { defaultValue: '系统提示词' }) },
        type: 'textarea',
        get placeholder() { return t('components:pipeline.param.systemPrompt.placeholder', { defaultValue: '把 {input} 翻译成中文（{input} 占位符引用上游输入）' }) },
      },
      {
        name: 'temperature',
        get label() { return t('components:pipeline.param.temperature', { defaultValue: '温度 (temperature)' }) },
        type: 'number',
        defaultValue: 0.7,
        min: 0,
        max: 2,
        step: 0.1,
        hint: '0 – 2',
      },
      {
        name: 'max_tokens',
        get label() { return t('components:pipeline.param.maxTokens') },
        type: 'number',
        defaultValue: 2048,
        min: 1,
        step: 1,
        hint: '≥ 1',
      },
      {
        name: 'output_format',
        get label() { return t('components:pipeline.param.outputFormat', { defaultValue: '输出格式' }) },
        type: 'select',
        options: ['text', 'json'],
        defaultValue: 'text',
      },
    ],
  },
}

export const BUILTIN_LIST: BuiltinDef[] = [
  BUILTIN_DEFS.file_input,
  BUILTIN_DEFS.file_output,
  BUILTIN_DEFS.ffmpeg,
  BUILTIN_DEFS.llm,
]

// ============================================================
// 外部 API 节点（遗留：§6.7 起由 llm builtin 取代，不进 palette、
// 保存/执行均已拦截。参数全部不被后端读取，故置空 —— §6.1 决策）
// ============================================================

export const EXTERNAL_PARAMS: ParamSpec[] = []

// ============================================================
// 分类视觉（图标 + 强调色；仅视觉用途，不再参与能力推导）
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
  /** 模块分类（仅视觉：图标/配色；能力不再由分类推导） */
  category: string
  /**
   * 模块能力声明（ModuleResponse.capabilities 数据驱动转换）。
   * undefined/空 = B5 过渡期或无能力模块 → 卡片展示兜底空态（不崩）。
   */
  capabilities?: CapabilityDef[]
  /** 当前选中能力裸名（§6.2 节点 capability 字段） */
  capabilityId: string
  capabilityLabel: string
  /** 变体 pin（§6.2 节点 model 字段；空/undefined = 跟随激活变体，执行前校验） */
  model?: string
  /** 设备软约束（§6.2 节点 device 字段；空/'auto' = 自动分配；未知设备仅警告回退，不阻断） */
  device?: string
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
      const cap = selectedCapability(data)
      if (cap) return { inputs: cap.inputs, outputs: cap.outputs }
      const fallback = moduleCapability(data.category)
      return { inputs: fallback.inputs, outputs: fallback.outputs }
    }
    case 'builtin': {
      const def = BUILTIN_DEFS[data.builtin]
      return { inputs: def.inputs, outputs: def.outputs }
    }
    case 'external':
      return {
        inputs: [{ id: 'in', label: t('components:pipeline.port.input'), dataType: 'any' }],
        outputs: [{ id: 'out', label: t('components:pipeline.port.output'), dataType: 'any' }],
      }
  }
}

export function getParamSpecs(data: PipelineNodeData): ParamSpec[] {
  switch (data.kind) {
    case 'module':
      // 数据驱动：按所选能力的 manifest params schema 渲染；无能力 → 空表
      return selectedCapability(data)?.params ?? []
    case 'builtin':
      return BUILTIN_DEFS[data.builtin].params
    case 'external':
      return EXTERNAL_PARAMS
  }
}

export function nodeKindLabel(data: PipelineNodeData): string {
  switch (data.kind) {
    case 'module':
      return t('components:pipeline.nodeKind.module', { label: data.capabilityLabel })
    case 'builtin':
      return t('components:pipeline.nodeKind.builtin', { label: BUILTIN_DEFS[data.builtin].label })
    case 'external':
      return t('components:pipeline.nodeKind.external', { method: data.method })
  }
}

// ============================================================
// 拖拽载荷与节点创建
// ============================================================

export const DRAG_MIME = 'application/reactflow'

export interface DragPayload {
  nodeType: 'module' | 'builtin' | 'external'
  moduleId?: string
  moduleName?: string
  moduleVersion?: string
  category?: string
  /** 模块能力声明（ModuleResponse.capabilities 原样透传；B5 过渡期可缺失/为 null） */
  capabilities?: CapabilityDecl[] | null
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

/** 由拖拽载荷创建节点（含默认参数快照；模块节点能力数据驱动） */
export function createPipelineNode(
  payload: DragPayload,
  position: { x: number; y: number },
): PipelineFlowNode {
  if (payload.nodeType === 'module') {
    const category = payload.category ?? 'other'
    const caps = capabilitiesFromModule(payload.capabilities)
    const first = caps[0] ?? null
    const data: ModuleNodeData = {
      kind: 'module',
      label: payload.moduleName ?? payload.moduleId ?? t('components:pipeline.unnamedModule'),
      moduleId: payload.moduleId ?? 'unknown',
      moduleVersion: payload.moduleVersion ?? '0.1.0',
      category,
      capabilities: caps,
      capabilityId: first?.id ?? '',
      capabilityLabel: first?.label ?? '',
      status: 'waiting',
      params: first ? defaultParams(first.params) : {},
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
    label: t('components:pipeline.external.title'),
    endpoint: 'https://api.example.com/v1/process',
    method: 'POST',
    status: 'waiting',
    params: defaultParams(EXTERNAL_PARAMS),
  }
  return { id: uid('external'), type: 'external', position, data }
}

// ============================================================
// §6.2 契约序列化辅助（C3 toSpec/fromSpec 消费）
// ============================================================

/**
 * ffmpeg args 归一为数组（null 安全）。遗留字符串形状按空白拆词
 * （与后端 B7 防御性拆分的语义一致；编辑态保真，不做 trim/过滤）。
 */
export function normalizeStringArrayParam(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.filter((v): v is string => typeof v === 'string')
  }
  if (typeof value === 'string') {
    const trimmed = value.trim()
    return trimmed ? trimmed.split(/\s+/) : []
  }
  return []
}

/** ffmpeg args 写入 spec 前的序列化：恒数组，剔除空白条目（P0-2） */
export function serializeArgsParam(value: unknown): string[] {
  return normalizeStringArrayParam(value)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
}

/**
 * 模块节点 → §6.2 契约字段（toSpec 时展开进 PipelineNodeSpec）。
 * capability 为裸名；model/device 仅在设置时写出（TOML 无 null，
 * 后端 Option skip_serializing_if；device='auto' 等价缺省，不写）。
 */
export function moduleNodeSpecFields(data: ModuleNodeData): {
  capability: string
  model?: string
  device?: string
} {
  const out: { capability: string; model?: string; device?: string } = {
    capability: data.capabilityId,
  }
  const model = (data.model ?? '').trim()
  if (model) out.model = model
  const device = (data.device ?? '').trim()
  if (device && device !== 'auto') out.device = device
  return out
}

/** moduleNodeSpecFields 的逆操作（fromSpec 时恢复 ModuleNodeData 字段） */
export function moduleNodeFieldsFromSpec(spec: {
  capability?: string | null
  model?: string | null
  device?: string | null
}): { capabilityId: string; model?: string; device?: string } {
  const capabilityId = (spec.capability ?? '').trim()
  const model = (spec.model ?? '').trim()
  const device = (spec.device ?? '').trim()
  return {
    capabilityId,
    ...(model ? { model } : {}),
    ...(device ? { device } : {}),
  }
}

// ============================================================
// 参数字段渲染（ParamSpec 全类型；C3 NodeParamsPanel 可整体委托）
// ============================================================

export interface StringArrayFieldProps {
  /** 当前值（数组；兼容遗留字符串形状，自动归一） */
  value: ParamValue | undefined
  onChange: (value: string[]) => void
  /** 单条参数输入框占位提示 */
  placeholder?: string
}

/** ffmpeg args 数组化编辑器（P0-2）：逐条增删改，值恒为 string[] */
export function StringArrayField({ value, onChange, placeholder }: StringArrayFieldProps) {
  const { t: tc } = useTranslation('components')
  const items = normalizeStringArrayParam(value)
  const updateAt = (index: number, next: string) => {
    onChange(items.map((v, i) => (i === index ? next : v)))
  }
  const removeAt = (index: number) => {
    onChange(items.filter((_, i) => i !== index))
  }
  return (
    <div className="space-y-1.5">
      {items.length === 0 && (
        <p className="rounded-md border border-dashed border-border px-2.5 py-2 text-[11px] text-muted-foreground">
          {tc('pipeline.param.args.empty', { defaultValue: '尚无参数，点击下方按钮逐条添加' })}
        </p>
      )}
      {items.map((item, index) => (
        <div key={index} className="flex items-center gap-1.5">
          <Input
            className="h-7 flex-1 font-mono text-xs"
            value={item}
            placeholder={placeholder}
            onChange={(e) => updateAt(index, e.target.value)}
          />
          <Button
            variant="ghost"
            size="icon-xs"
            onClick={() => removeAt(index)}
            aria-label={tc('pipeline.param.args.removeAria', { defaultValue: '删除该参数' })}
            title={tc('pipeline.param.args.removeAria', { defaultValue: '删除该参数' })}
          >
            <Trash2 className="h-3 w-3" />
          </Button>
        </div>
      ))}
      <Button variant="outline" size="xs" onClick={() => onChange([...items, ''])}>
        <Plus className="h-3 w-3" />
        {tc('pipeline.param.args.add', { defaultValue: '添加参数' })}
      </Button>
    </div>
  )
}

export interface ParamSpecFieldProps {
  spec: ParamSpec
  value: ParamValue | undefined
  onChange: (value: ParamValue) => void
}

/**
 * 通用参数字段渲染：string / textarea / number(min/max/step) / boolean /
 * select / string_array 全类型覆盖（manifest schema 与 llm builtin 共用）。
 */
export function ParamSpecField({ spec, value, onChange }: ParamSpecFieldProps) {
  const { t: tc } = useTranslation('components')
  const numberValue = typeof value === 'number' && Number.isFinite(value) ? value : ''
  const stringValue = typeof value === 'string' ? value : value === undefined ? '' : String(value)
  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium">
          {spec.label}
          {spec.required && (
            <span className="ml-0.5 text-status-error" aria-hidden>
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
      {spec.type === 'textarea' && (
        <textarea
          className="min-h-20 w-full resize-y rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs outline-none transition-colors placeholder:text-muted-foreground focus-visible:border-ring focus-visible:shadow-[0_0_0_3px_var(--ring-glow)]"
          rows={4}
          value={stringValue}
          placeholder={spec.placeholder}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
      {spec.type === 'number' && (
        <Input
          type="number"
          step={spec.step ?? 'any'}
          min={spec.min}
          max={spec.max}
          className="h-8 font-mono text-xs"
          value={numberValue}
          placeholder={spec.placeholder}
          onChange={(e) => onChange(e.target.value === '' ? '' : Number(e.target.value))}
        />
      )}
      {spec.type === 'boolean' && (
        <div className="flex items-center justify-between rounded-md border border-border bg-muted/30 px-3 py-2">
          <span className="text-xs text-muted-foreground">
            {spec.placeholder ?? tc('pipeline:params.enable', { defaultValue: '启用' })}
          </span>
          <Switch checked={value === true} onCheckedChange={(checked) => onChange(checked)} />
        </div>
      )}
      {spec.type === 'select' && (
        <Select value={stringValue} onValueChange={(v) => onChange(v)}>
          <SelectTrigger className="h-8 w-full text-xs">
            <SelectValue placeholder={tc('pipeline:params.selectPlaceholder', { defaultValue: '请选择' })} />
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
      {spec.type === 'string_array' && (
        <StringArrayField value={value} onChange={onChange} placeholder={spec.placeholder} />
      )}
    </div>
  )
}

// ============================================================
// 模块节点：能力选择 + 变体 pin / 设备软约束编辑器
// （C3 NodeParamsPanel 挂载；数据经 props 传入，本组件不自行拉取）
// ============================================================

export interface CapabilitySelectProps {
  data: ModuleNodeData
  /** 切换能力：调用方应同时按新能力的 params 重建默认参数 */
  onChange: (capabilityId: string) => void
}

/** 模块节点能力选择（裸名列表来自 manifest）。无能力 → 兜底空态提示。 */
export function CapabilitySelect({ data, onChange }: CapabilitySelectProps) {
  const { t: tc } = useTranslation('components')
  const caps = data.capabilities ?? []
  if (caps.length === 0) {
    return (
      <p className="rounded-md border border-dashed border-border px-2.5 py-2 text-[11px] text-muted-foreground">
        {tc('pipeline.module.noCapabilities', { defaultValue: '该模块未声明任何能力（manifest capabilities 缺失）' })}
      </p>
    )
  }
  if (caps.length === 1) {
    const only = caps[0]!
    return (
      <div className="space-y-1.5">
        <span className="text-xs font-medium">
          {tc('pipeline.module.capabilityLabel', { defaultValue: '能力' })}
        </span>
        <p className="rounded-md border border-border bg-muted/30 px-3 py-2 font-mono text-xs">
          {only.id}
        </p>
        {only.description && (
          <p className="text-[10px] text-muted-foreground">{only.description}</p>
        )}
      </div>
    )
  }
  const value = caps.some((c) => c.id === data.capabilityId) ? data.capabilityId : caps[0]!.id
  return (
    <div className="space-y-1.5">
      <span className="text-xs font-medium">
        {tc('pipeline.module.capabilityLabel', { defaultValue: '能力' })}
      </span>
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger className="h-8 w-full text-xs">
          <SelectValue placeholder={tc('pipeline.module.pickCapability', { defaultValue: '选择能力' })} />
        </SelectTrigger>
        <SelectContent>
          {caps.map((cap) => (
            <SelectItem key={cap.id} value={cap.id} className="text-xs" title={cap.description}>
              <span className="font-mono">{cap.id}</span>
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

/** 变体 pin 下拉中表示「跟随激活变体」的哨兵值（Radix Select 不接受空串 value） */
export const VARIANT_FOLLOW_ACTIVE = '__follow_active__'

export interface ModuleBindingEditorProps {
  /** 当前 pin 的变体（§6.2 node.model；空 = 跟随激活变体） */
  model?: string
  /** 当前设备软约束（§6.2 node.device；空/'auto' = 自动分配） */
  device?: string
  /** 该模块的变体列表（model_id，来自 models API / use-models） */
  variants: string[]
  /** 本机设备列表（/api/devices，如 useDevices().devices ?? []） */
  devices: DeviceResponse[]
  onChange: (patch: { model?: string; device?: string }) => void
}

/**
 * 模块节点执行绑定编辑器：变体 pin（缺省 = 跟随激活变体）+ device 软约束
 * （auto + 本机设备列表）。未知设备仅警告提示，不阻断（§6.2 软约束语义）。
 */
export function ModuleBindingEditor({ model, device, variants, devices, onChange }: ModuleBindingEditorProps) {
  const { t: tc } = useTranslation('components')
  const modelValue = model && model.trim() ? model : VARIANT_FOLLOW_ACTIVE
  const deviceValue = device && device.trim() ? device : 'auto'
  const deviceIds = devices.map((d) => d.id)
  const deviceKnown = deviceValue === 'auto' || deviceIds.includes(deviceValue)
  // 未知设备仍列入选项（软约束：展示 + 警告，不回写清空）。
  // 选项展示「设备名 + 支持栈」：物理归并后条目即物理卡，名称比裸 id 更可读
  const deviceOptions: { value: string; label?: string; stacks?: string[] }[] = [
    ...devices.map((d) => ({
      value: d.id,
      label: d.name !== d.id ? d.name : undefined,
      stacks: d.stacks?.length ? d.stacks : undefined,
    })),
  ]
  if (!deviceKnown) {
    deviceOptions.unshift({ value: deviceValue })
  }

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <span className="text-xs font-medium">
          {tc('pipeline.module.variantPin', { defaultValue: '变体 pin (model)' })}
        </span>
        <Select
          value={modelValue}
          onValueChange={(v) => onChange({ model: v === VARIANT_FOLLOW_ACTIVE ? '' : v })}
        >
          <SelectTrigger className="h-8 w-full text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={VARIANT_FOLLOW_ACTIVE} className="text-xs">
              {tc('pipeline.module.variantFollowActive', { defaultValue: '跟随激活变体' })}
            </SelectItem>
            {variants.map((v) => (
              <SelectItem key={v} value={v} className="text-xs">
                <span className="font-mono">{v}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-[10px] text-muted-foreground">
          {tc('pipeline.module.variantPin.hint', { defaultValue: '缺省跟随激活变体；执行前校验 pin 与激活是否一致' })}
        </p>
      </div>

      <div className="space-y-1.5">
        <span className="text-xs font-medium">{t('components:pipeline.param.device')}</span>
        <Select
          value={deviceValue}
          onValueChange={(v) => onChange({ device: v === 'auto' ? '' : v })}
        >
          <SelectTrigger className="h-8 w-full text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="auto" className="text-xs">
              {tc('pipeline.module.deviceAuto', { defaultValue: 'auto（调度器自动分配）' })}
            </SelectItem>
            {deviceOptions.map((d) => (
              <SelectItem key={d.value} value={d.value} className="text-xs">
                <span className="flex items-baseline gap-2">
                  <span className="font-mono">{d.value}</span>
                  {d.label && <span className="truncate text-muted-foreground">{d.label}</span>}
                  {d.stacks && d.stacks.length > 1 && (
                    <span className="ml-auto font-mono text-[10px] uppercase text-muted-foreground/70">
                      {d.stacks.join('+')}
                    </span>
                  )}
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {!deviceKnown && (
          <p className="text-[11px] text-status-starting">
            {tc('pipeline.module.deviceUnknown', {
              defaultValue: '本机未检测到该设备；执行时将警告并回退 auto（软约束，不阻断）',
            })}
          </p>
        )}
      </div>
    </div>
  )
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
        // 玻璃拟态节点卡：半透明底 + 内发光描边；四态描边以 important 覆盖玻璃描边
        'glass-card w-56 rounded-lg border-2! text-card-foreground',
        meta.border,
        meta.glow,
        selected
          ? 'node-card-selected'
          : 'transition-shadow duration-200 hover:shadow-lg',
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
      title={t('components:pipeline.nodeStatusTitle', { status: meta.label })}
      className={cn('h-2 w-2 shrink-0 rounded-full transition-colors', meta.dot)}
    />
  )
}

/** 端口行：handle 锚定在本行（relative），左侧输入 / 右侧输出，标签按数据类型着色 */
function PortRow({ inputs, outputs }: { inputs: Port[]; outputs: Port[] }) {
  return (
    <div className="relative flex h-7 items-center justify-between rounded-b-md border-t border-border-glow bg-background/40 px-2.5">
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
        title={t('components:pipeline.portTitle', { type: meta.label })}
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

/** 模块节点：模块名 + 数据驱动能力 + 变体/设备绑定徽标，状态色边框 */
export function ModuleNode({ data, selected }: NodeProps<ModuleFlowNode>) {
  const cap = selectedCapability(data)
  const fallback = moduleCapability(data.category)
  const visual = categoryVisual(data.category)
  const Icon = visual.icon
  const hasCapabilities = (data.capabilities ?? []).length > 0

  const subtitle = cap
    ? `${cap.label} · ${categoryLabel(data.category)}`
    : hasCapabilities
      ? t('components:pipeline.module.noCapabilitySelected', { defaultValue: '未选择能力' })
      : t('components:pipeline.module.noCapabilities', { defaultValue: '未声明能力' })

  const deviceChip = data.device && data.device !== 'auto' ? data.device : null
  const bindingChips = [data.model || null, deviceChip].filter((v): v is string => !!v)

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
          <p
            className={cn(
              'truncate text-[10px]',
              cap ? 'text-muted-foreground' : 'text-status-starting',
            )}
            title={subtitle}
          >
            {subtitle}
          </p>
        </div>
        <StatusDot status={data.status} />
      </div>
      <div className="truncate border-t border-border/70 px-3 py-1 font-mono text-[10px] text-muted-foreground">
        {data.moduleId}@{data.moduleVersion}
        {bindingChips.length > 0 && (
          <span className="ml-1.5 text-primary" title={bindingChips.join(' · ')}>
            {bindingChips.join(' · ')}
          </span>
        )}
      </div>
      <PortRow
        inputs={cap?.inputs ?? fallback.inputs}
        outputs={cap?.outputs ?? fallback.outputs}
      />
    </NodeCard>
  )
}

/** 内置节点：file_input / file_output / ffmpeg / llm */
export function BuiltinNode({ data, selected }: NodeProps<BuiltinFlowNode>) {
  const def = BUILTIN_DEFS[data.builtin]
  const Icon = def.icon
  let preview: string | null = null
  if (data.builtin === 'ffmpeg') {
    // P0-2：args 数组形状（兼容遗留字符串）
    const args = normalizeStringArrayParam(data.params.args)
    preview = args.length > 0 ? args.join(' ') : null
  } else if (data.builtin === 'llm') {
    preview = typeof data.params.model === 'string' && data.params.model ? data.params.model : null
  } else {
    preview = typeof data.params.path === 'string' && data.params.path ? data.params.path : null
  }
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
          {t('components:pipeline.builtin.badge')}
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

/** 外部 API 节点（遗留画布展示；§6.7 起已由 llm builtin 取代，不可新建） */
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
          <p className="truncate text-[10px] text-muted-foreground">
            {t('components:pipeline.external.description')}
          </p>
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
        inputs={[{ id: 'in', label: t('components:pipeline.port.input'), dataType: 'any' }]}
        outputs={[{ id: 'out', label: t('components:pipeline.port.output'), dataType: 'any' }]}
      />
    </NodeCard>
  )
}

export const pipelineNodeTypes: NodeTypes = {
  module: ModuleNode,
  builtin: BuiltinNode,
  external: ExternalApiNode,
}
