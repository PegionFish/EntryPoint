import type { JSX } from 'react'
import type { CapabilityParamSchema } from '@/api/types'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

/** 直跑参数表单：按 ParamSchema 类型渲染控件，提交前按类型归一 */
export function ParamField({
  name,
  schema,
  value,
  onChange,
}: {
  name: string
  schema: CapabilityParamSchema
  value: unknown
  onChange: (value: unknown) => void
}): JSX.Element {
  const enumValues = schema.enum ?? schema.options ?? null
  const type = (schema.type || 'string').toLowerCase()

  if (enumValues && enumValues.length > 0) {
    const current = value === undefined || value === null ? '' : String(value)
    return (
      <Select value={current} onValueChange={(v) => onChange(v)}>
        <SelectTrigger className="w-full">
          <SelectValue placeholder={name} />
        </SelectTrigger>
        <SelectContent>
          {enumValues.map((opt) => (
            <SelectItem key={opt} value={opt}>
              {opt}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    )
  }

  if (type === 'boolean') {
    const current = value === undefined || value === null ? '' : String(value)
    return (
      <Select value={current} onValueChange={(v) => onChange(v === 'true')}>
        <SelectTrigger className="w-full">
          <SelectValue placeholder={name} />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="true">true</SelectItem>
          <SelectItem value="false">false</SelectItem>
        </SelectContent>
      </Select>
    )
  }

  if (type === 'integer' || type === 'float' || type === 'number') {
    return (
      <Input
        type="number"
        value={value === undefined || value === null ? '' : String(value)}
        min={schema.min ?? undefined}
        max={schema.max ?? undefined}
        step={schema.step ?? (type === 'integer' ? 1 : 'any')}
        onChange={(e) => {
          const raw = e.target.value
          if (raw === '') {
            onChange(undefined)
            return
          }
          const num =
            type === 'integer' ? Number.parseInt(raw, 10) : Number.parseFloat(raw)
          onChange(Number.isNaN(num) ? raw : num)
        }}
        className="font-mono text-xs"
      />
    )
  }

  return (
    <Input
      value={value === undefined || value === null ? '' : String(value)}
      onChange={(e) => onChange(e.target.value)}
      className="font-mono text-xs"
    />
  )
}
