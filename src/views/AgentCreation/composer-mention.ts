/** 只处理光标所在的提及范围，选择结果时不会删除后面的正文或其他提及。 */
export interface ComposerMention {
  trigger: '@' | '$'
  start: number
  end: number
  query: string
}

export function mentionAt(text: string, start: number, end = start): ComposerMention | null {
  if (start !== end) return null
  const prefix = text.slice(0, start)
  const match = /([@$])([^@$\n\r，。；！？,;!?]*)$/u.exec(prefix)
  if (!match) return null
  const previous = prefix[match.index - 1]
  // 邮箱、标识符中的 @/$ 仍是普通文字；中文句子里可以直接写“请用@拆书”。
  if (previous && /[A-Za-z0-9_./+-]/.test(previous)) return null
  return { trigger: match[1] as ComposerMention['trigger'], start: match.index, end: start, query: match[2] }
}

export function removeMention(text: string, mention: ComposerMention): { text: string; caret: number } {
  return { text: text.slice(0, mention.start) + text.slice(mention.end), caret: mention.start }
}
