import { agentCommand } from '@/agent/client'
import { isTauriRuntime } from './writing-storage'
import { withRecordStore } from './desktop-records'

/**
 * 本书备忘的前端门面：数据只有一份，在宿主里。
 * 规则是任务规则集里的作者规则；决定与偏好是作品记忆里的决定（偏好按"作者偏好"类别区分，
 * 作者层记忆建好后再挪）。妙笔、补全、工作流、微信工具照旧经这里读，函数名不变。
 * 旧版存在前端记录库里的备忘在首次读取时迁进宿主，只迁一次。
 */

export type AiNoteKind = 'rule' | 'decision' | 'preference'
export type AiNoteSource = 'author' | 'miaobi' | 'workflow'

export interface AiBookNote {
  id: string
  bookId: string
  kind: AiNoteKind
  text: string
  source: AiNoteSource | string
  createTime: string
  /** 规则的类别或记忆的类别 */
  category?: string
}

export const NOTE_KIND_LABEL: Record<AiNoteKind, string> = { rule: '规则', decision: '决定', preference: '偏好' }

const LEGACY_DB = 'ew-local-ai-notes'
const LEGACY_STORE = 'kv'
const migrated = new Set<string>()

/** 测试用：忘掉本进程里"已检查过旧备忘"的记录 */
export const resetNotesMigration = () => migrated.clear()

/** 旧备忘只迁一次：宿主按作品记标记，前端只负责把旧记录读出来交过去 */
const importLegacyNotes = async (bookId: string) => {
  if (migrated.has(bookId)) return
  migrated.add(bookId)
  try {
    const stored = await withRecordStore<unknown>(LEGACY_DB, LEGACY_STORE, undefined, 'readonly', store => store.get(`notes:${bookId}`))
    if (!Array.isArray(stored) || !stored.length) return
    await agentCommand('import_legacy_notes', { bookId, notes: stored })
  } catch (error) {
    console.warn('迁移旧本书备忘失败，下次打开再试', error)
    migrated.delete(bookId)
  }
}

export const listBookNotes = async (bookId: string | number): Promise<AiBookNote[]> => {
  if (!isTauriRuntime() || bookId == null || bookId === '') return []
  const key = String(bookId)
  await importLegacyNotes(key)
  return agentCommand<AiBookNote[]>('canon_query', { bookId: key, view: 'notes' })
}

export const addBookNote = async (data: { bookId: string | number; kind: AiNoteKind; text: string; source?: AiNoteSource; category?: string }) => {
  const text = String(data.text || '').trim()
  if (!text) throw new Error('备忘内容不能为空')
  if (!isTauriRuntime()) throw new Error('本书备忘需要桌面端')
  return agentCommand<unknown>('add_note', {
    bookId: String(data.bookId),
    kind: data.kind,
    text,
    source: data.source || 'author',
    category: data.category,
  })
}

export const deleteBookNote = async (data: { bookId: string | number; id: string }) => {
  await agentCommand('delete_note', { id: data.id })
}

/** 改文字或改类别：宿主里规则与记忆是两种记录，改动按"删旧加新"落 */
export const updateBookNote = async (data: { bookId: string | number; id: string; text?: string; kind?: AiNoteKind }) => {
  const notes = await listBookNotes(data.bookId)
  const current = notes.find(note => note.id === data.id)
  if (!current) throw new Error('备忘不存在')
  await deleteBookNote({ bookId: data.bookId, id: data.id })
  return addBookNote({ bookId: data.bookId, kind: data.kind || current.kind, text: data.text ?? current.text, source: current.source as AiNoteSource })
}

export const buildNotesContext = async (bookId: string | number | undefined, budget = 1500): Promise<string> => {
  if (bookId == null || bookId === '') return ''
  let notes: AiBookNote[]
  try {
    notes = await listBookNotes(bookId)
  } catch (error) {
    console.warn('读取本书备忘失败', error)
    return ''
  }
  if (!notes.length) return ''
  const order: AiNoteKind[] = ['rule', 'decision', 'preference']
  const sorted = [...notes].sort((a, b) => order.indexOf(a.kind) - order.indexOf(b.kind) || b.createTime.localeCompare(a.createTime))
  const lines: string[] = []
  let used = 0
  let omitted = 0
  for (const note of sorted) {
    const line = `- [${NOTE_KIND_LABEL[note.kind]}] ${note.text}`
    if (used + line.length > budget) {
      omitted += 1
      continue
    }
    lines.push(line)
    used += line.length
  }
  return ['【本书备忘】作者定下的规则与决定，生成和回答时必须遵守，不要与之相悖。', ...lines, omitted ? `（另有 ${omitted} 条备忘因篇幅省略）` : ''].filter(Boolean).join('\n')
}
