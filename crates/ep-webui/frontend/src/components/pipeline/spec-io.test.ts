import type { Edge } from '@xyflow/react'
import { describe, expect, it } from 'vitest'

import type { ModuleResponse, PipelineSpec } from '@/api/types'
import type {
  ModuleFlowNode,
  NodeParams,
  PipelineFlowNode,
} from '@/components/shared/pipeline-node'
import {
  canvasFingerprint,
  fromSpec,
  parsePipelineToml,
  specToToml,
  toSpec,
} from './spec-io'

// ============================================================
// T-28a：管线 TOML 导入导出 round-trip（§6.4）
//
// 链路：画布 → toSpec → specToToml → parsePipelineToml → fromSpec → toSpec，
// 首尾两次 spec 以 canonicalJson（键序无关）逐字段比对，覆盖：
// - params 保真（含 UI 不展示字段）；
// - file_gate media 平铺键 ↔ 嵌套对象（§5.6）；
// - ffmpeg args 恒数组（P0-2）与模板节点 ffmpeg_template 桥接（T-13）；
// - module 节点 §6.2 契约字段（capability/model/device）；
// - 端口名 in/out ↔ input/output 归一（§6.4）。
// ============================================================

const META = { id: 'demo-pipe', name: '演示管线', description: 'round-trip fixture' }

/** mock 模块列表（deep-filter: denoise 能力 + 变体表，供 fromSpec 水合） */
const MODULE_LIST: ModuleResponse[] = [
  {
    id: 'deep-filter',
    name: 'DeepFilter',
    version: '0.5.6',
    category: 'denoise',
    status: 'stopped',
    capabilities: [
      { name: 'denoise', description: '', input_type: 'audio', output_type: 'audio' },
    ],
  } as unknown as ModuleResponse,
]

function builtinNode(
  id: string,
  builtin: 'file_input' | 'file_output' | 'ffmpeg' | 'file_gate',
  params: NodeParams,
  position: { x: number; y: number },
): PipelineFlowNode {
  return {
    id,
    type: 'builtin',
    position,
    data: { kind: 'builtin', builtin, label: id, status: 'waiting', params },
  }
}

function moduleNode(
  id: string,
  overrides?: Partial<Extract<ModuleFlowNode['data'], { kind: 'module' }>>,
): PipelineFlowNode {
  return {
    id,
    type: 'module',
    position: { x: 300, y: 60 },
    data: {
      kind: 'module',
      label: 'DeepFilter 降噪',
      moduleId: 'deep-filter',
      moduleVersion: '0.5.6',
      category: 'denoise',
      capabilityId: 'denoise',
      capabilityLabel: 'denoise',
      status: 'waiting',
      params: { quality: 0.5 },
      ...overrides,
    },
  }
}

function edge(source: string, target: string, index: number): Edge {
  return {
    id: `e${index}`,
    source,
    target,
    sourceHandle: 'out',
    targetHandle: 'in',
  }
}

/** 画布 → spec → TOML → spec（导出→导入链路） */
function roundTripSpec(nodes: PipelineFlowNode[], edges: Edge[]): PipelineSpec {
  const spec = toSpec(nodes, edges, META)
  const toml = specToToml(spec, [])
  return parsePipelineToml(toml)
}

describe('spec-io: 管线 TOML 导入导出 round-trip（§6.4）', () => {
  it('builtin + module 混合管线：两次 toSpec 逐字段一致', () => {
    const nodes: PipelineFlowNode[] = [
      builtinNode('in', 'file_input', { path: '/workspace/uploads/a.wav' }, { x: 0, y: 0 }),
      moduleNode('den', { model: 'deep-filter.mycorp.df3@small', device: 'cuda:1' }),
      builtinNode('out', 'file_output', { path: '/workspace/output/a-clean.wav' }, { x: 600, y: 0 }),
    ]
    const edges = [edge('in', 'den', 0), edge('den', 'out', 1)]

    const first = toSpec(nodes, edges, META)
    const reparsed = roundTripSpec(nodes, edges)
    const second = toSpec(
      fromSpec(reparsed, MODULE_LIST).nodes,
      fromSpec(reparsed, MODULE_LIST).edges,
      META,
    )

    expect(second).toEqual(first)
  })

  it('file_gate media 平铺键（§5.6）与 ffmpeg args 恒数组（P0-2）往返不丢失', () => {
    const nodes: PipelineFlowNode[] = [
      builtinNode('in', 'file_input', { path: 'store:uploads' }, { x: 0, y: 0 }),
      builtinNode('gate', 'file_gate', {
        extensions: ['mp4', 'mkv'],
        media_min_duration_secs: 3.5,
        media_min_width: 1280,
        min_size_bytes: 1024,
        on_mismatch: 'skip',
      }, { x: 200, y: 0 }),
      builtinNode('ff', 'ffmpeg', {
        args: ['-i', '{input}', '-vf', 'negate', '{output}'],
        output_extension: 'mp4',
      }, { x: 400, y: 0 }),
      builtinNode('out', 'file_output', { path: '/tmp/out.mp4' }, { x: 600, y: 0 }),
    ]
    const edges = [
      edge('in', 'gate', 0),
      edge('gate', 'ff', 1),
      edge('ff', 'out', 2),
    ]

    const first = toSpec(nodes, edges, META)
    // 首次 toSpec 已把平铺 media_* 收敛为嵌套 media 对象
    const gateSpec = first.nodes.find((n) => n.id === 'gate')!
    expect(gateSpec.params.media).toEqual({ min_duration_secs: 3.5, min_width: 1280 })
    expect(gateSpec.params.media_min_duration_secs).toBeUndefined()

    const canvas = fromSpec(roundTripSpec(nodes, edges), MODULE_LIST)
    // 中间画布态：file_gate 还原平铺键、ffmpeg args 保持数组
    const gateData = canvas.nodes.find((n) => n.id === 'gate')!
    if (gateData.data.kind !== 'builtin') throw new Error('expect builtin')
    expect(gateData.data.params.media_min_duration_secs).toBe(3.5)
    expect(gateData.data.params.media).toBeUndefined()
    const ffData = canvas.nodes.find((n) => n.id === 'ff')!
    if (ffData.data.kind !== 'builtin') throw new Error('expect builtin')
    expect(ffData.data.params.args).toEqual(['-i', '{input}', '-vf', 'negate', '{output}'])

    const second = toSpec(canvas.nodes, canvas.edges, META)
    expect(second).toEqual(first)
  })

  it('T-13 ffmpeg 模板节点：ffmpeg_template 桥接往返不丢失', () => {
    const nodes: PipelineFlowNode[] = [
      builtinNode('in', 'file_input', { path: '/a.wav' }, { x: 0, y: 0 }),
      builtinNode('tpl', 'ffmpeg', {
        template: 'wav_to_mp3',
        bitrate: '192k',
      }, { x: 200, y: 0 }),
    ]
    const edges = [edge('in', 'tpl', 0)]

    const first = toSpec(nodes, edges, META)
    expect(first.nodes.find((n) => n.id === 'tpl')!.builtin).toBe('ffmpeg_template')

    const canvas = fromSpec(roundTripSpec(nodes, edges), MODULE_LIST)
    const tplData = canvas.nodes.find((n) => n.id === 'tpl')!
    if (tplData.data.kind !== 'builtin') throw new Error('expect builtin')
    expect(tplData.data.builtin).toBe('ffmpeg') // 画布仍以 ffmpeg builtin 承载
    expect(tplData.data.params.template).toBe('wav_to_mp3')

    expect(toSpec(canvas.nodes, canvas.edges, META)).toEqual(first)
  })

  it('手写 TOML（[nodes.params] 段式）可解析且端口名归一为 in/out', () => {
    const toml = `
[pipeline]
id = "hand-made"
name = "手写样例"
description = ""

[[nodes]]
id = "src"
kind = "builtin"
builtin = "file_input"
label = "文件输入"
position = { x = 10, y = 20 }

[nodes.params]
path = "/workspace/uploads/a.wav"

[[nodes]]
id = "den"
kind = "module"
module_id = "deep-filter"
capability = "denoise"
model = "deep-filter.mycorp.df3@small"
device = "cuda:0"
label = "降噪"
position = { x = 30, y = 40 }

[nodes.params]
quality = 0.5

[[edges]]
from = ["src", "output"]
to = ["den", "input"]
`.trim()

    const spec = parsePipelineToml(toml)
    expect(spec.pipeline.id).toBe('hand-made')
    expect(spec.nodes).toHaveLength(2)
    expect(spec.edges).toEqual([{ from: ['src', 'output'], to: ['den', 'input'] }])

    const canvas = fromSpec(spec, MODULE_LIST)
    expect(canvas.nodes).toHaveLength(2)
    expect(canvas.skippedNodes).toBe(0)
    // 服务端端口名（input/output）归一为前端 handle id（in/out），否则边会被静默丢弃
    expect(canvas.edges[0]?.sourceHandle).toBe('out')
    expect(canvas.edges[0]?.targetHandle).toBe('in')
    const den = canvas.nodes.find((n) => n.id === 'den')!
    if (den.data.kind !== 'module') throw new Error('expect module')
    expect(den.data.capabilityId).toBe('denoise')
    expect(den.data.model).toBe('deep-filter.mycorp.df3@small')
    expect(den.data.device).toBe('cuda:0')
  })

  it('解析失败带可读错误：缺 [pipeline] id / 非法 kind', () => {
    expect(() => parsePipelineToml('[[nodes]]\nid = "a"\nkind = "builtin"')).toThrow(
      /缺少 \[pipeline\] id/,
    )
    expect(() =>
      parsePipelineToml('[pipeline]\nid = "x"\n\n[[nodes]]\nid = "a"\nkind = "ghost"'),
    ).toThrow(/kind 非法或缺失/)
  })

  it('未知 builtin 节点跳过并计数；引用未知节点的边被过滤', () => {
    const toml = `
[pipeline]
id = "future"
name = "future"

[[nodes]]
id = "known"
kind = "builtin"
builtin = "file_input"
label = "in"

[[nodes]]
id = "ghost"
kind = "builtin"
builtin = "time_warp"
label = "未来节点"

[[edges]]
from = ["known", "output"]
to = ["ghost", "input"]
`.trim()

    const canvas = fromSpec(parsePipelineToml(toml), MODULE_LIST)
    expect(canvas.nodes.map((n) => n.id)).toEqual(['known'])
    expect(canvas.skippedNodes).toBe(1)
    expect(canvas.edges).toHaveLength(0)
  })

  it('canvasFingerprint：能力水合元数据不进指纹（不误报未保存更改）', () => {
    const spec = parsePipelineToml(`
[pipeline]
id = "fp"
name = "fp"

[[nodes]]
id = "den"
kind = "module"
module_id = "deep-filter"
capability = "denoise"
label = "降噪"
params = { quality = 0.5 }

[[nodes]]
id = "out"
kind = "builtin"
builtin = "file_output"
label = "out"

[[edges]]
from = ["den", "output"]
to = ["out", "input"]
`.trim())
    const canvas = fromSpec(spec, MODULE_LIST)
    const hydrated = fromSpec(spec, MODULE_LIST)
    // 模拟模块列表异步水合：改 capabilities/版本/分类（非契约字段）
    const denHydrated = hydrated.nodes.find((n) => n.id === 'den')
    if (denHydrated?.data.kind !== 'module') throw new Error('expect module')
    denHydrated.data.capabilities = [
      {
        id: 'denoise',
        label: 'denoise',
        description: '',
        inputs: [],
        outputs: [],
        params: [],
      },
    ]
    denHydrated.data.moduleVersion = '9.9.9'
    expect(canvasFingerprint(hydrated.nodes, hydrated.edges, META)).toBe(
      canvasFingerprint(canvas.nodes, canvas.edges, META),
    )
  })
})
