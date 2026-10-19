import { agentCommand } from './client'
import { appSettings, flushAppSettings } from '@/storage/app-settings'
import { isTauriRuntime } from '@/storage/writing-storage'

/**
 * 旧版妙笔（2026-10-13 之前）的对话记录存在前端自己的记录库里；妙笔换成 Agent 的视图后，
 * 把它们搬进宿主会话：原样显示、标成"旧版妙笔记录"只读，作者要接着聊就开新对话。
 * 每本书只搬一次（按书记标记），宿主按旧对话编号去重，搬过的不会重复；旧记录原地保留，不删。
 */

const FLAG_PREFIX = 'ew-miaobi-legacy-imported:'

export interface LegacyChatImportOutcome {
  books: number
  imported: number
}

const imported = (bookId: string) => appSettings.getItem(`${FLAG_PREFIX}${bookId}`) === '1'

const toMillis = (value: string | undefined) => {
  const time = Date.parse(String(value || ''))
  return Number.isFinite(time) ? time : undefined
}

/** 把一本书的旧对话交给宿主；返回这次新搬进去的条数 */
export const importLegacyMiaobiChats = async (bookId: string): Promise<number> => {
  if (!isTauriRuntime() || !bookId || imported(bookId)) return 0
  const { getLocalChatHistory, listLocalChatSessions } = await import('@/storage/local-ai-chat')
  const { data: sessions } = await listLocalChatSessions({ bookId })
  let count = 0
  if (sessions.length) {
    const payload = []
    for (const session of sessions) {
      const { data: messages } = await getLocalChatHistory({ sessionId: String(session.id) })
      payload.push({
        id: String(session.id),
        title: session.title,
        createdAt: toMillis(session.createTime),
        updatedAt: toMillis(session.updateTime) ?? toMillis(session.createTime),
        messages: messages.map(message => ({ role: message.role, text: message.content, createdAt: toMillis(message.createTime) })),
      })
    }
    const result = await agentCommand<{ imported: number }>('import_legacy_chats', { bookId, sessions: payload })
    count = result.imported
  }
  appSettings.setItem(`${FLAG_PREFIX}${bookId}`, '1')
  await flushAppSettings()
  return count
}

/** 书架上每本书都搬一遍（只在第一次进 Agent 板块时跑一次；每本书各自记标记） */
export const importAllLegacyMiaobiChats = async (): Promise<LegacyChatImportOutcome> => {
  if (!isTauriRuntime()) return { books: 0, imported: 0 }
  const { getLocalLibraryStorage } = await import('@/storage/local-library')
  const books = await getLocalLibraryStorage().listLocalBooks({ sortBy: 'updateTime', sortOrder: 'DESC' })
  let total = 0
  let touched = 0
  for (const book of books) {
    const id = String(book.id)
    if (imported(id)) continue
    touched += 1
    try {
      total += await importLegacyMiaobiChats(id)
    } catch (error) {
      console.warn(`旧妙笔对话搬迁失败，下次再试：《${book.title}》`, error)
    }
  }
  return { books: touched, imported: total }
}
