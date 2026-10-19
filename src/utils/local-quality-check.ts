import type { WorkflowQualityIssue, WorkflowQualityNotice } from '@/types/workflow'
import type { QualityReport } from '@/types/canon'
import { scanSensitiveText } from '@/storage/local-sensitive-words'
import { countWords } from '@/utils/word-count'

/**
 * 逐章生文的本地规则质检（服务端"规则 + AI 评审"双轨里的规则轨）。
 *
 * 开源版不跑 AI 评审（每章多一次付费调用，收益不稳），critic 如实标 unavailable。
 * 规则收得很紧：只拦"明显写坏了"的硬伤（字数严重不足、整段复读），
 * 敏感词和字数偏多只作提示不拦截——拦截意味着生成停机等确认，误拦比漏报更伤。
 */

const WORD_LOW_BLOCK_RATIO = 0.55
const WORD_HIGH_NOTICE_RATIO = 1.7
const REPEAT_MIN_PARAGRAPH_CHARS = 16
const QUOTE_MAX = 3
const QUOTE_SLICE = 60

const clipQuote = (value: string) => {
  const text = String(value || '').trim()
  return text.length > QUOTE_SLICE ? `${text.slice(0, QUOTE_SLICE)}…` : text
}

const checkWordCount = (words: number, targetWords: number): WorkflowQualityIssue[] => {
  if (!targetWords) return []
  if (words < Math.round(targetWords * WORD_LOW_BLOCK_RATIO)) {
    return [{
      source: 'rule',
      code: 'word_count_low',
      dimension: '篇幅',
      message: `本章只有 ${words} 字，明显低于目标 ${targetWords} 字，疑似生成中断或提前收尾`,
      severity: 'high',
      blocking: true,
      fix: '可选择"重写本章"补足剧情，或人工补写后接受',
      metrics: { wordCount: words, targetWords },
    }]
  }
  if (words > Math.round(targetWords * WORD_HIGH_NOTICE_RATIO)) {
    return [{
      source: 'rule',
      code: 'word_count_high',
      dimension: '篇幅',
      message: `本章 ${words} 字，超出目标 ${targetWords} 字较多，节奏可能偏拖`,
      severity: 'low',
      blocking: false,
      metrics: { wordCount: words, targetWords },
    }]
  }
  return []
}

const checkParagraphRepeat = (text: string): WorkflowQualityIssue[] => {
  const seen = new Map<string, number>()
  const repeated: string[] = []
  for (const raw of String(text || '').split(/\n+/)) {
    const paragraph = raw.trim()
    if (paragraph.length < REPEAT_MIN_PARAGRAPH_CHARS) continue
    const count = (seen.get(paragraph) || 0) + 1
    seen.set(paragraph, count)
    if (count === 2) repeated.push(paragraph)
  }
  if (!repeated.length) return []
  return [{
    source: 'rule',
    code: 'paragraph_repeat',
    dimension: '重复',
    message: `发现 ${repeated.length} 处整段重复内容，正文疑似复读`,
    severity: 'high',
    blocking: true,
    quotes: repeated.slice(0, QUOTE_MAX).map(clipQuote),
    fix: '建议"重写本章"，或手动删去重复段后接受',
  }]
}

const checkSensitiveWords = (text: string): WorkflowQualityIssue[] => {
  const result = scanSensitiveText(text)
  if (!result.hasSensitive) return []
  const top = result.matches.slice(0, 5)
  const quotes: string[] = []
  for (const item of top) {
    if (quotes.length >= QUOTE_MAX) break
    const index = text.indexOf(item.word)
    if (index === -1) continue
    quotes.push(clipQuote(text.slice(Math.max(0, index - 12), index + item.word.length + 24)))
  }
  return [{
    source: 'rule',
    code: 'sensitive_words',
    dimension: '敏感词',
    message: `命中本地敏感词 ${result.total} 处：${top.map(item => `${item.word}×${item.count}`).join('、')}`,
    severity: 'low',
    blocking: false,
    quotes,
    fix: '发布前可在编辑器里用敏感词检查逐处替换',
  }]
}

export const runLocalChapterQualityCheck = (params: {
  chapterId: number
  chapterNo: number
  chapterTitle: string
  text: string
  targetWords: number
  contentVersion: number
  modelCode?: string
}): WorkflowQualityNotice => {
  const words = countWords(params.text)
  const issues = [
    ...checkWordCount(words, params.targetWords),
    ...checkParagraphRepeat(params.text),
    ...checkSensitiveWords(params.text),
  ]
  return {
    version: 1,
    requiresAction: issues.some(issue => issue.blocking),
    chapterId: params.chapterId,
    chapterNo: params.chapterNo,
    chapterTitle: params.chapterTitle,
    issues,
    wordCount: words,
    contentVersion: params.contentVersion,
    modelCode: params.modelCode,
    createdAt: new Date().toISOString(),
    critic: { status: 'unavailable', error: '开源版暂未接入 AI 评审，仅做规则检查' },
  }
}

/**
 * 并入 Agent 质量门（写作台“本章体检”同一套）：状态疑点、钩子、AI 味、人物表等。
 * 字数、整段重复、敏感词本地已按工作流目标字数查过，宿主的 structure.* 不重复计入。
 * block 仍会停机等确认；suspect 与 notice 只作提示，不拦截生成。
 */
export const mergeAgentGate = (
  notice: WorkflowQualityNotice,
  report: Pick<QualityReport, 'issues'> | null | undefined
): WorkflowQualityNotice => {
  const extra: WorkflowQualityIssue[] = (report?.issues || [])
    .filter(issue => !issue.code.startsWith('structure.'))
    .map(issue => ({
      source: issue.source,
      code: issue.code,
      dimension: issue.dimension,
      message: issue.message,
      severity: issue.level === 'notice' ? 'low' : 'high',
      blocking: issue.level === 'block',
      fix: issue.fix,
      paragraphs: issue.paragraphs,
      quotes: issue.quotes.slice(0, QUOTE_MAX).map(clipQuote),
    }))
  if (!extra.length) return notice
  const issues = [...notice.issues, ...extra]
  return { ...notice, issues, requiresAction: issues.some(issue => issue.blocking) }
}
