import type { Edge } from '@xyflow/react'

import type {
  ModuleResponse,
  PipelineEdgeSpec,
  PipelineNodeSpec,
  PipelineSpec,
} from '@/api/types'
import i18n from '@/i18n'
import {
  BUILTIN_DEFS,
  DATA_TYPE_META,
  capabilitiesFromModule,
  getNodePorts,
  moduleNodeFieldsFromSpec,
  moduleNodeSpecFields,
  serializeArgsParam,
} from '@/components/shared/pipeline-node'
import type {
  BuiltinFlowNode,
  BuiltinKind,
  ModuleFlowNode,
  ModuleNodeData,
  NodeParams,
  ParamValue,
  PipelineFlowNode,
  PipelineNodeData,
} from '@/components/shared/pipeline-node'

/**
 * 模块级（非组件上下文）取 pipeline 命名空间文案。
 * 用于模块顶层执行的纯函数 toSpec/fromSpec/specToToml 等无法调用 hook 的位置。
 * （T-28a 自 pipeline.tsx 抽出为纯函数模块，供单测直接消费。）
 */
function tp(key: string, options?: Record<string, unknown>): string {
  return i18n.t(key, { ns: 'pipeline', ...(options ?? {}) })
}

// ============================================================
// spec ↔ React Flow 互转（纯函数）
// ============================================================

/** 管线 id 命名规则（与后端 PUT /api/pipelines/:id 校验一致） */
export const PIPELINE_ID_RULE = /^[a-z0-9][a-z0-9-]*$/

export interface CanvasMeta {
  id: string
  name: string
  description: string
}

/**
 * P1-11 节点级超时/重试的前端挂载字段（后端 SpecNode 的 timeout_secs /
 * retry_count）。前端暂无编辑 UI，仅加载→保存往返保真（C2 节点数据类型
 * 不含这两个字段，页面层以交叉类型读写）。
 */
export interface NodeTimingExt {
  timeoutSecs?: number
  retryCount?: number
}

/** 深度排序对象键后序列化，保证相同内容产生相同指纹（与键插入顺序无关） */
export function canonicalJson(value: unknown): string {
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
export function toSpec(
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
        // T-13：ffmpeg 模板节点（params.template）在服务端契约里是
        // builtin = "ffmpeg_template"，模板 id 与参数原样下发（执行器零模型成本
        // 按模板表渲染命令）；无模板的普通 ffmpeg 节点保持原样。
        if (n.data.builtin === 'ffmpeg') {
          const templateId = typeof params.template === 'string' ? params.template.trim() : ''
          if (templateId) {
            return { ...common, params, kind: 'builtin', builtin: 'ffmpeg_template' }
          }
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
export function buildBudgetSpec(
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
export function cascadeLayout(spec: PipelineSpec): Map<string, { x: number; y: number }> {
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
export const FILE_GATE_MEDIA_KEYS: [flat: string, nested: string][] = [
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
export function normalizePortName(
  port: string | undefined,
  fallback: 'in' | 'out',
): string {
  if (!port) return fallback
  if (port === 'input' || port === 'in') return 'in'
  if (port === 'output' || port === 'out') return 'out'
  return port
}

/** 边标签：按上游节点输出端口的数据类型取展示名（画布连线/加载时使用） */
export function edgeLabelFor(
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

/** 未保存画布按 spec 执行时的兜底 pipeline.id（后端要求非空） */
export function fallbackPipelineId(name: string): string {
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
export function collectDependencyLines(nodes: PipelineFlowNode[]): string[] {
  const lines: string[] = []
  for (const n of nodes) {
    if (n.data.kind !== 'module') continue
    const pin = (n.data.model ?? '').trim()
    if (pin) lines.push(`${pin}（节点 ${n.id} · 模块 ${n.data.moduleId}）`)
  }
  return lines
}

/**
 * 管线 spec → TOML 文本（§6.4 导出）：头部注释依赖清单 +
 * `[pipeline]` / `[[nodes]]` / `[[edges]]` 标准布局（与后端 bridge 输出口径一致，
 * 可被 ep_core::load_pipeline 直接读回）。
 */
export function specToToml(spec: PipelineSpec, dependencyLines: string[]): string {
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
export function parsePipelineToml(text: string): PipelineSpec {
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
export function fromSpec(
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
      // T-13：执行器模板节点在画布上以 ffmpeg builtin 呈现（端口/handle 沿用
      // BUILTIN_DEFS.ffmpeg），模板 id 保留在 params.template，保存时由 toSpec
      // 桥接回 ffmpeg_template，保证 加载→保存 往返不丢失。
      if (sn.builtin === 'ffmpeg_template') {
        const templateId = typeof params.template === 'string' ? params.template.trim() : ''
        const data: PipelineNodeData = {
          kind: 'builtin',
          builtin: 'ffmpeg',
          label:
            sn.label ||
            (templateId
              ? tp(`builtinTemplate.${templateId}.name`, { defaultValue: templateId })
              : BUILTIN_DEFS.ffmpeg.label),
          status: 'waiting',
          params,
        }
        applyTiming(data, sn)
        nodes.push({ id: sn.id, type: 'builtin', position, data })
        known.add(sn.id)
        continue
      }
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
export function canvasFingerprint(
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
