/**
 * 微信回复排版：微信只认纯文本，模型回复里的 Markdown 要拍平；
 * 单条消息有长度上限，长回复按段落切成几条发。
 */

/** 单条消息的字符上限（微信未公开上限，取保守值，实测可调） */
export const WEIXIN_SEGMENT_MAX_CHARS = 1500
/** 一次回复最多发几条，防止长篇大论刷屏 */
export const WEIXIN_SEGMENT_MAX_COUNT = 5
const TRUNCATED_SUFFIX = '……（内容较长，已截断）'

/** Markdown → 纯文本：去标题井号、粗斜体、代码围栏、链接语法；列表符号换成中文习惯的「·」 */
export const markdownToPlainText = (markdown: string): string => {
  let text = String(markdown || '').replace(/\r\n?/g, '\n')
  // 代码围栏只去掉围栏行，保留里面的内容
  text = text.replace(/^\s*```[^\n]*\n?/gm, '')
  // 表格：分隔行去掉，单元格竖线换成「 | 」
  text = text.replace(/^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/gm, '')
  text = text.replace(/^\s*\|(.*)\|\s*$/gm, (_match, row: string) =>
    row.split('|').map(cell => cell.trim()).filter(Boolean).join(' | ')
  )
  // 标题
  text = text.replace(/^\s{0,3}#{1,6}\s+/gm, '')
  // 引用
  text = text.replace(/^\s{0,3}>\s?/gm, '')
  // 链接与图片：保留文字，附上地址
  text = text.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, '$1')
  text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_match, label: string, url: string) =>
    label.trim() === url.trim() ? url : `${label}（${url}）`
  )
  // 粗体、斜体、删除线、行内代码
  text = text.replace(/\*\*\*([^*]+)\*\*\*/g, '$1')
  text = text.replace(/\*\*([^*]+)\*\*/g, '$1')
  text = text.replace(/__([^_]+)__/g, '$1')
  text = text.replace(/(^|[^\w*])\*([^*\n]+)\*(?!\w)/g, '$1$2')
  text = text.replace(/~~([^~]+)~~/g, '$1')
  text = text.replace(/`([^`\n]+)`/g, '$1')
  // 无序列表符号
  text = text.replace(/^(\s*)[-*+]\s+/gm, '$1· ')
  // 水平线
  text = text.replace(/^\s*([-*_]\s*){3,}$/gm, '')
  // 连续空行压成一个
  text = text.replace(/\n{3,}/g, '\n\n')
  return text.trim()
}

/** 按段落切分，尽量不在句子中间断开；超过条数上限的部分丢弃并提示 */
export const splitForWeixin = (
  text: string,
  maxChars = WEIXIN_SEGMENT_MAX_CHARS,
  maxCount = WEIXIN_SEGMENT_MAX_COUNT
): string[] => {
  const normalized = String(text || '').trim()
  if (!normalized) return []
  if (normalized.length <= maxChars) return [normalized]

  const segments: string[] = []
  let current = ''
  const push = () => {
    if (current.trim()) segments.push(current.trim())
    current = ''
  }
  for (const paragraph of normalized.split(/\n{2,}/)) {
    const pieces = paragraph.length > maxChars ? hardSplit(paragraph, maxChars) : [paragraph]
    for (const piece of pieces) {
      // 装不下就先把攒好的发出去；hardSplit 保证单块不超限
      if (current && current.length + 2 + piece.length > maxChars) push()
      current = current ? `${current}\n\n${piece}` : piece
    }
  }
  push()

  if (segments.length <= maxCount) return segments
  const kept = segments.slice(0, maxCount)
  const last = kept[maxCount - 1]
  kept[maxCount - 1] = (last.length + TRUNCATED_SUFFIX.length > maxChars
    ? last.slice(0, maxChars - TRUNCATED_SUFFIX.length)
    : last) + TRUNCATED_SUFFIX
  return kept
}

/** 单段超长：先按句末标点切，再按行切，最后按字数硬切 */
const hardSplit = (paragraph: string, maxChars: number): string[] => {
  const out: string[] = []
  let buffer = ''
  const sentences = paragraph.split(/(?<=[。！？!?；;\n])/)
  for (const sentence of sentences) {
    if (sentence.length > maxChars) {
      if (buffer) out.push(buffer)
      buffer = ''
      for (let index = 0; index < sentence.length; index += maxChars) {
        out.push(sentence.slice(index, index + maxChars))
      }
      continue
    }
    if ((buffer + sentence).length > maxChars) {
      out.push(buffer)
      buffer = sentence
    } else {
      buffer += sentence
    }
  }
  if (buffer) out.push(buffer)
  return out.map(item => item.trim()).filter(Boolean)
}
