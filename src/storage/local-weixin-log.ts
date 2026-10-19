import { withRecordStore } from './desktop-records'
import { createLocalEntityId, nowIso } from './local-library-utils'

/**
 * 微信助手·对话记录（给作者看的那份）。
 *
 * 一条微信消息一条记录：收到的原话、怎么处理的（命令 / 助手 / 确认执行）、中间调了哪些工具、
 * 用了多久、出了什么错；回出去的每条消息也各记一条，并指回它回答的那条入站消息。
 *
 * 与会话里的"对话记忆"（local-weixin-bot 的 history）是两回事：那份是喂给模型的多轮上下文，
 * 条数有限、「/清空」会清；这份是账本，只给作者回看，不进模型，只在设置页里手动清。
 * 与 AI 调用记录同一存储通道，只留最近 500 条。排障用的逐事件技术日志另见
 * ai/channels/weixin/logfile.ts，写在应用数据目录的文件里。
 */

export type WeixinLogDirection = 'in' | 'out'

/** 入站：这条消息走了哪条路；出站：这条回复是谁发的 */
export type WeixinLogRoute =
  | 'command'
  | 'assistant'
  | 'confirm'
  | 'cancel'
  | 'push'
  | 'notice'
  | 'media'
  | 'rejected'
  | 'system'

export type WeixinLogKind = 'text' | 'voice' | 'image' | 'file' | 'video' | 'other' | 'push'

export interface WeixinLogStep {
  tool: string
  label: string
  status: 'done' | 'error'
  durationMs: number
  chars: number
  error?: string
}

export interface WeixinLogEntry {
  id: number
  /** ISO 时间 */
  at: string
  direction: WeixinLogDirection
  kind: WeixinLogKind
  /** 原文；媒体是文件名 */
  text: string
  route: WeixinLogRoute
  ok: boolean
  error?: string
  /** 入站：从收到到回复发出的总耗时 */
  durationMs?: number
  /** 入站：助手本轮调过的工具 */
  steps?: WeixinLogStep[]
  /** 入站：回答用的模型名 */
  model?: string
  /** 出站：回答的是哪条入站记录 */
  replyTo?: number
  /** 附加说明：事实核对替换、放弃的待确认操作、命令名等 */
  note?: string
}

export type WeixinLogInput = Omit<WeixinLogEntry, 'id' | 'at'> & { at?: string }

const DB_NAME = 'ew-weixin-log'
const STORE_NAME = 'entries'
export const WEIXIN_LOG_MAX_ENTRIES = 500
/** 超出上限多少条才清一次：每次落账都做全量扫描太浪费 */
const TRIM_SLACK = 40
/** 单条正文保留的字数：续写预览可能几千字，账本里留个头就够回看 */
const TEXT_KEEP_CHARS = 4000
const ERROR_KEEP_CHARS = 500

const withStore = <T>(mode: IDBTransactionMode, run: Parameters<typeof withRecordStore>[4]) =>
  withRecordStore<T>(DB_NAME, STORE_NAME, 'id', mode, run)

const clip = (value: unknown, max: number) => String(value ?? '').slice(0, max)

const normalize = (input: WeixinLogInput): Omit<WeixinLogEntry, 'id'> => ({
  at: input.at || nowIso(),
  direction: input.direction,
  kind: input.kind,
  text: clip(input.text, TEXT_KEEP_CHARS),
  route: input.route,
  ok: input.ok !== false,
  ...(input.error ? { error: clip(input.error, ERROR_KEEP_CHARS) } : {}),
  ...(input.durationMs != null ? { durationMs: Math.max(0, Math.round(input.durationMs)) } : {}),
  ...(input.steps?.length ? { steps: input.steps.map(normalizeStep) } : {}),
  ...(input.model ? { model: clip(input.model, 120) } : {}),
  ...(input.replyTo ? { replyTo: input.replyTo } : {}),
  ...(input.note ? { note: clip(input.note, 300) } : {}),
})

const normalizeStep = (step: WeixinLogStep): WeixinLogStep => ({
  tool: clip(step.tool, 80),
  label: clip(step.label, 200),
  status: step.status === 'error' ? 'error' : 'done',
  durationMs: Math.max(0, Math.round(step.durationMs || 0)),
  chars: Math.max(0, Math.round(step.chars || 0)),
  ...(step.error ? { error: clip(step.error, ERROR_KEEP_CHARS) } : {}),
})

const byNewest = (a: WeixinLogEntry, b: WeixinLogEntry) => String(b.at).localeCompare(String(a.at)) || b.id - a.id

/** 落一条；返回带 id 的记录，入站记录处理完后用 updateWeixinLog 补结果 */
export const appendWeixinLog = async (input: WeixinLogInput): Promise<WeixinLogEntry> => {
  const entry: WeixinLogEntry = { ...normalize(input), id: createLocalEntityId() }
  await withStore('readwrite', store => store.put(entry))
  await trimIfNeeded()
  return entry
}

/** 补写处理结果：找不到（已被清掉）就静默跳过 */
export const updateWeixinLog = async (id: number, patch: Partial<WeixinLogInput>): Promise<WeixinLogEntry | null> => {
  const existing = await withStore<WeixinLogEntry | undefined>('readonly', store => store.get(id))
  if (!existing) return null
  const merged: WeixinLogEntry = { ...normalize({ ...existing, ...patch, at: existing.at }), id: existing.id }
  await withStore('readwrite', store => store.put(merged))
  return merged
}

/** 最新在前 */
export const listWeixinLog = async (limit = WEIXIN_LOG_MAX_ENTRIES): Promise<WeixinLogEntry[]> => {
  const all = await withStore<WeixinLogEntry[]>('readonly', store => store.getAll())
  return all.sort(byNewest).slice(0, Math.max(1, limit))
}

export const clearWeixinLog = async (): Promise<number> => {
  const keys = await withStore<IDBValidKey[]>('readonly', store => store.getAllKeys())
  for (const key of keys) {
    await withStore('readwrite', store => store.delete(key))
  }
  return keys.length
}

const trimIfNeeded = async () => {
  const all = await withStore<WeixinLogEntry[]>('readonly', store => store.getAll())
  if (all.length <= WEIXIN_LOG_MAX_ENTRIES + TRIM_SLACK) return
  const doomed = all.sort(byNewest).slice(WEIXIN_LOG_MAX_ENTRIES)
  for (const item of doomed) {
    await withStore('readwrite', store => store.delete(item.id))
  }
}

/** 导出成纯文本，作者要发给别人看时用 */
export const formatWeixinLogAsText = (entries: WeixinLogEntry[]): string => {
  const lines: string[] = []
  for (const entry of [...entries].sort((a, b) => -byNewest(a, b))) {
    const who = entry.direction === 'in' ? '作者' : entry.route === 'push' ? '推送' : '助手'
    const state = entry.ok ? '' : `【失败：${entry.error || '未知错误'}】`
    lines.push(`[${entry.at.replace('T', ' ').slice(0, 19)}] ${who}${state}：${entry.text}`)
    for (const step of entry.steps || []) {
      lines.push(`    · ${step.label || step.tool}（${step.durationMs}ms，${step.chars} 字${step.error ? `，出错：${step.error}` : ''}）`)
    }
    if (entry.note) lines.push(`    （${entry.note}）`)
  }
  return lines.join('\n')
}
