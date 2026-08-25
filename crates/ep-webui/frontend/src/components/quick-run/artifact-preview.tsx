/** 直跑产物预览（文本 / 图片；其余类型仅下载） */
export interface ArtifactPreview {
  nodeId: string
  name: string
  kind: 'text' | 'image' | 'binary'
  text?: string
  objectUrl?: string
  size: number
}

export const TEXT_PREVIEW_EXTS = /\.(txt|json|srt|vtt|ass|csv|log|md|toml)$/i
export const IMAGE_PREVIEW_EXTS = /\.(png|jpe?g|webp|gif|bmp)$/i
/** 预览大小上限（超过仅下载） */
const PREVIEW_MAX_BYTES = 2 * 1024 * 1024

export async function fetchArtifactPreview(
  url: string,
  nodeId: string,
  name: string,
): Promise<ArtifactPreview> {
  const resp = await fetch(url)
  if (!resp.ok) throw new Error(`API ${resp.status}`)
  // P1：先读 Content-Length 预判体积，超限直接取消 body 放弃预览。
  // 避免 await resp.blob() 把数 GB 产物全量下载进内存造成内存峰值。
  const headerLen = Number(resp.headers.get('Content-Length'))
  if (Number.isFinite(headerLen) && headerLen > PREVIEW_MAX_BYTES) {
    void resp.body?.cancel()
    return { nodeId, name, kind: 'binary', size: headerLen }
  }
  const blob = await resp.blob()
  if (blob.size > PREVIEW_MAX_BYTES) {
    return { nodeId, name, kind: 'binary', size: blob.size }
  }
  if (IMAGE_PREVIEW_EXTS.test(name)) {
    return {
      nodeId,
      name,
      kind: 'image',
      objectUrl: URL.createObjectURL(blob),
      size: blob.size,
    }
  }
  if (TEXT_PREVIEW_EXTS.test(name)) {
    return { nodeId, name, kind: 'text', text: await blob.text(), size: blob.size }
  }
  return { nodeId, name, kind: 'binary', size: blob.size }
}
