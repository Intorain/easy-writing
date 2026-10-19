import { listLocalCharacters } from '@/storage/local-reference'
import { aiFactsOpenForeshadows, aiFactsQuery, type AiFact } from '@/storage/ai-facts'
import { canUseAiIndex } from '@/storage/ai-index'
import { buildCharacterContext } from '@/storage/character-context'
import { buildDraftReviewMessages, buildDraftReviseMessages } from '@/config/ai-prompts'
import { promptTemperature } from '@/storage/local-prompts'
import { LONG_COMPLETION_TIMEOUT_MS, requestLocalChatCompletion } from '@/utils/local-ai-client'
import { parseAiJson } from '@/utils/ai-json'
import { getCharacterNames, matchCharacterNames } from '@/utils/character-aliases'
import { loadBookChapters } from '../book/chapters'
import { buildNotesContext } from '@/storage/ai-notes'

/**
 * 生成—校验—修订。
 *
 * 校验分两层：代码先把草稿里出场的人物、它们截至上一章的已知状态、本书规则收齐；
 * 再交给模型做"只找冲突"的审查，输出结构化问题清单。有实质问题（high/medium）才修订，
 * 修订要求最小改动，结果长度异常则放弃修订保留原稿——校验永远不能把稿子弄丢。
 */

export type IssueType = 'name' | 'state' | 'timeline' | 'logic' | 'rule' | 'other'
export type IssueSeverity = 'high' | 'medium' | 'low'

export interface DraftIssue {
  type: IssueType
  severity: IssueSeverity
  text: string
  quote: string
  suggestion: string
}

export interface KnownState {
  /** 草稿里出场的角色正式名 */
  characters: string[]
  /** 这些角色截至上一章的状态（给模型看的文本） */
  stateText: string
  /** 角色资料（复用角色匹配与预算截断） */
  characterText: string
  /** 截至上一章未回收的伏笔 */
  foreshadowText: string
  facts: number
}

export type VerifyStatus = 'ok' | 'skipped' | 'failed'

export interface VerifyResult {
  status: VerifyStatus
  issues: DraftIssue[]
  known: KnownState
  error?: string
}

const ISSUE_TYPES: IssueType[] = ['name', 'state', 'timeline', 'logic', 'rule', 'other']
const SEVERITIES: IssueSeverity[] = ['high', 'medium', 'low']
const MAX_ISSUES = 8
const MIN_DRAFT_CHARS = 40
const STATE_BUDGET = 2400

const squash = (text: string) => String(text || '').replace(/\s+/g, '').replace(/[“”"‘’'「」『』]/g, '')

export const describeChapterOrder = (fact: AiFact) => (fact.storyOrder ? `第${fact.storyOrder}章` : '较早章节')

/** 按主体汇总状态事实：每个状态项只留最新值 */
export const summarizeStateFacts = (facts: AiFact[], budget = STATE_BUDGET) => {
  const bySubject = new Map<string, Map<string, AiFact>>()
  for (const fact of facts) {
    if (fact.kind !== 'state' && fact.kind !== 'relation') continue
    const key = fact.kind === 'relation' ? `关系·${fact.object.split('：')[0] || fact.object}` : fact.predicate
    const entries = bySubject.get(fact.subject) || new Map<string, AiFact>()
    entries.set(key, fact)
    bySubject.set(fact.subject, entries)
  }
  const lines: string[] = []
  let used = 0
  for (const [subject, entries] of bySubject) {
    const line = `${subject}：${[...entries.values()].map(fact => `${fact.kind === 'relation' ? '关系' : fact.predicate}=${fact.object}（${describeChapterOrder(fact)}）`).join('；')}`
    if (used + line.length > budget) {
      lines.push('（更多状态因篇幅省略）')
      break
    }
    lines.push(line)
    used += line.length
  }
  return lines.join('\n')
}

export const collectKnownState = async (params: { bookId: string | number; chapterId?: number | null; draft: string }): Promise<KnownState> => {
  const { data } = await listLocalCharacters({ bookId: params.bookId })
  const matches = matchCharacterNames(data.list, params.draft)
  const selected = new Map<number, (typeof data.list)[number]>()
  for (const candidates of matches.values()) for (const character of candidates) selected.set(character.id, character)
  const characters = [...selected.values()].slice(0, 8)
  const known: KnownState = {
    characters: characters.map(character => character.name),
    stateText: '',
    characterText: buildCharacterContext(data.list, params.draft),
    foreshadowText: '',
    facts: 0,
  }
  if (!canUseAiIndex() || !characters.length) return known
  // "截至上一章"：本章自己旧版本抽出的事实不能用来否定本章的新稿
  const chapters = await loadBookChapters(params.bookId)
  const index = chapters.findIndex(chapter => chapter.id === Number(params.chapterId))
  const until = index > 0 ? chapters[index - 1].id : index === 0 ? null : (params.chapterId ?? null)
  if (index === 0) return known
  const subjects = characters.flatMap(character => getCharacterNames(character))
  const [facts, open] = await Promise.all([
    aiFactsQuery(params.bookId, { subjects, kinds: ['state', 'relation'], untilChapterId: until, limit: 120 }),
    aiFactsOpenForeshadows(params.bookId, until),
  ])
  known.facts = facts.length
  known.stateText = summarizeStateFacts(facts)
  known.foreshadowText = open.slice(-6).map(fact => `${describeChapterOrder(fact)}：${fact.text}`).join('\n')
  return known
}

export const normalizeIssues = (raw: unknown, draft: string): DraftIssue[] => {
  const list = Array.isArray((raw as { issues?: unknown })?.issues) ? ((raw as { issues: unknown[] }).issues) : []
  const haystack = squash(draft)
  const issues: DraftIssue[] = []
  for (const item of list) {
    if (issues.length >= MAX_ISSUES) break
    const entry = (item && typeof item === 'object' ? item : {}) as Record<string, unknown>
    const text = String(entry.text || '').trim()
    if (!text) continue
    const type = ISSUE_TYPES.includes(String(entry.type) as IssueType) ? (String(entry.type) as IssueType) : 'other'
    const severity = SEVERITIES.includes(String(entry.severity) as IssueSeverity) ? (String(entry.severity) as IssueSeverity) : 'low'
    const quoteRaw = String(entry.quote || '').trim()
    const quote = quoteRaw && haystack.includes(squash(quoteRaw)) ? quoteRaw.slice(0, 200) : ''
    issues.push({ type, severity, text: text.slice(0, 200), quote, suggestion: String(entry.suggestion || '').trim().slice(0, 300) })
  }
  const rank: Record<IssueSeverity, number> = { high: 0, medium: 1, low: 2 }
  return issues.sort((a, b) => rank[a.severity] - rank[b.severity])
}

export const needsRevision = (issues: DraftIssue[]) => issues.some(issue => issue.severity !== 'low')

export const formatIssues = (issues: DraftIssue[]) =>
  issues
    .map((issue, index) => `${index + 1}. [${issue.severity}/${issue.type}] ${issue.text}${issue.quote ? `\n   原文：“${issue.quote}”` : ''}${issue.suggestion ? `\n   建议：${issue.suggestion}` : ''}`)
    .join('\n')

export const verifyDraft = async (params: {
  bookId: string | number
  chapterId?: number | null
  draft: string
  previousTail?: string
  modelCode: string
  rules?: string
  signal?: AbortSignal
}): Promise<VerifyResult> => {
  const empty: KnownState = { characters: [], stateText: '', characterText: '', foreshadowText: '', facts: 0 }
  if (params.draft.trim().length < MIN_DRAFT_CHARS) return { status: 'skipped', issues: [], known: empty, error: '草稿太短，无需核对' }
  let known = empty
  try {
    known = await collectKnownState(params)
  } catch (error) {
    console.warn('收集已知状态失败，仅按角色资料与规则核对', error)
  }
  const rules = [String(params.rules || '').trim(), await buildNotesContext(params.bookId)].filter(Boolean).join('\n\n')
  if (!known.characterText && !known.stateText && !rules && !String(params.previousTail || '').trim()) {
    return { status: 'skipped', issues: [], known, error: '没有可比对的资料' }
  }
  try {
    const raw = await requestLocalChatCompletion({
      modelCode: params.modelCode,
      scene: 'draft_review',
      sceneLabel: '正文核对',
      temperature: promptTemperature('draft-guard', 'reviewSystem'),
      timeoutMs: LONG_COMPLETION_TIMEOUT_MS,
      signal: params.signal,
      messages: buildDraftReviewMessages({
        draft: params.draft,
        previousTail: params.previousTail,
        knownState: [known.stateText, known.foreshadowText ? `未回收伏笔：\n${known.foreshadowText}` : ''].filter(Boolean).join('\n'),
        characters: known.characterText,
        rules,
      }),
    })
    return { status: 'ok', issues: normalizeIssues(parseAiJson(raw, ['issues']), params.draft), known }
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    return { status: 'failed', issues: [], known, error: error instanceof Error ? error.message : String(error) }
  }
}

export const reviseDraft = async (params: {
  draft: string
  issues: DraftIssue[]
  previousTail?: string
  modelCode: string
  rules?: string
  signal?: AbortSignal
}): Promise<string> => {
  const revised = (
    await requestLocalChatCompletion({
      modelCode: params.modelCode,
      scene: 'draft_revise',
      sceneLabel: '正文修订',
      temperature: promptTemperature('draft-guard', 'reviseSystem'),
      timeoutMs: LONG_COMPLETION_TIMEOUT_MS,
      signal: params.signal,
      messages: buildDraftReviseMessages({ draft: params.draft, issues: formatIssues(params.issues), previousTail: params.previousTail, rules: params.rules }),
    })
  ).trim()
  const ratio = revised.length / Math.max(params.draft.trim().length, 1)
  if (!revised || ratio < 0.6 || ratio > 1.6) throw new Error('修订结果篇幅异常，已保留原稿')
  return revised
}

export interface GuardOutcome {
  text: string
  issues: DraftIssue[]
  revised: boolean
  status: VerifyStatus
  error?: string
}

/** 校验 + 按需修订；任何失败都返回原稿 */
export const runDraftGuard = async (params: {
  bookId: string | number
  chapterId?: number | null
  draft: string
  previousTail?: string
  modelCode: string
  rules?: string
  signal?: AbortSignal
  onStage?: (stage: 'review' | 'revise') => void
}): Promise<GuardOutcome> => {
  params.onStage?.('review')
  const verified = await verifyDraft(params)
  if (verified.status !== 'ok' || !needsRevision(verified.issues)) {
    return { text: params.draft, issues: verified.issues, revised: false, status: verified.status, error: verified.error }
  }
  params.onStage?.('revise')
  try {
    const rules = [String(params.rules || '').trim(), await buildNotesContext(params.bookId)].filter(Boolean).join('\n\n')
    const text = await reviseDraft({ draft: params.draft, issues: verified.issues, previousTail: params.previousTail, modelCode: params.modelCode, rules, signal: params.signal })
    return { text, issues: verified.issues, revised: true, status: 'ok' }
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    return { text: params.draft, issues: verified.issues, revised: false, status: 'ok', error: error instanceof Error ? error.message : String(error) }
  }
}
