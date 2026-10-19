import { computed, getCurrentInstance, onUnmounted, ref, watch, type Ref } from 'vue'
import { appSettings, flushAppSettings } from '@/storage/app-settings'
import { useAgentWorkspace } from '@/views/AgentCreation/use-agent-workspace'
import { useWritingEditorStore } from '@/stores/writing-editor'
import { useAiChatStore } from '@/stores/ai-chat'
import type { AgentReference } from '@/types/agent-creation'

/**
 * 妙笔的会话：Agent 板块里那条作品会话在编辑器侧的视图。
 * 打开面板不建会话：这本书已有作品会话就接着用最近一条，没有就等作者第一次发消息再建（键 book:<bookId>，开新对话时加编号）。
 * 当前章作为引用随作者换章而换，编辑器里"添加到妙笔"的选区作为选段引用带上；对话、引导卡、成果都由 Agent 板块的组件渲染。
 */

const EPOCH_PREFIX = 'ew-miaobi-session-epoch:'
const anchorId = (bookId: string, chapterId: number) => `chapter:${bookId}:${chapterId}`
const clip = (text: string, max = 24) => (text.length > max ? `${text.slice(0, max)}…` : text)

export function useMiaobiSession(bookId: Ref<string>, bookTitle: Ref<string>) {
  const store = useAgentWorkspace()
  const editor = useWritingEditorStore()
  const aiChat = useAiChatStore()
  const ready = ref(false)
  const error = ref('')
  let opening: Promise<void> | null = null

  const epochOf = (id: string) => Number(appSettings.getItem(`${EPOCH_PREFIX}${id}`) || 0) || 0

  /** 当前章进引用：只保留一条章锚点，换章就换掉。会话还没建时记在待建会话的引用里 */
  const syncAnchor = () => {
    if (!bookId.value) return
    const chapterId = editor.activeChapterId
    const wanted = chapterId ? anchorId(bookId.value, chapterId) : null
    for (const reference of [...store.references]) {
      if (reference.kind === 'chapter' && reference.id.startsWith(`chapter:${bookId.value}:`) && reference.id !== wanted) {
        store.removeReference(reference.id)
      }
    }
    if (wanted && chapterId && !store.references.some(r => r.id === wanted)) {
      store.addReference({
        id: wanted,
        kind: 'chapter',
        bookId: bookId.value,
        chapterId: String(chapterId),
        title: `《${editor.activeChapterTitle || '当前章'}》`,
        description: '当前章',
        pinned: true,
      })
    }
  }

  /** 编辑器里"添加到妙笔"的选区 → 选段引用 */
  const drainSelections = () => {
    if (!bookId.value) return
    for (const selection of aiChat.consumeSelections()) {
      const content = selection.content.trim()
      if (!content) continue
      const reference: AgentReference = {
        id: `selection-${selection.id}`,
        kind: 'selection',
        bookId: bookId.value,
        chapterId: editor.activeChapterId ? String(editor.activeChapterId) : undefined,
        title: `${selection.title}${selection.range ? ` · ${selection.range}` : ''}选段`,
        description: clip(content),
        content,
      }
      store.addReference(reference)
    }
  }

  /** 面板只知道作品编号：书名从书架读，读不到就用编号 */
  const resolveTitle = async (id: string) => {
    if (bookTitle.value) return bookTitle.value
    try {
      const { getLocalLibraryStorage } = await import('@/storage/local-library')
      const book = await getLocalLibraryStorage().getLocalBookDetail(id)
      return book?.title || `作品 ${id}`
    } catch {
      return `作品 ${id}`
    }
  }

  const open = async (fresh = false) => {
    if (!bookId.value) return
    if (opening) return opening
    ready.value = false
    error.value = ''
    opening = (async () => {
      try {
        const id = bookId.value
        if (!store.ready) await store.initialize()
        // 这本书旧版妙笔的对话记录：第一次打开时搬进会话，作者在"对话"列表里能看到
        const { importLegacyMiaobiChats } = await import('@/agent/legacy-chats')
        if (await importLegacyMiaobiChats(id)) await store.reload()
        await store.prepareBookSession({ id, title: await resolveTitle(id) }, epochOf(id), fresh)
        syncAnchor()
        drainSelections()
        ready.value = true
      } catch (cause) {
        error.value = String((cause as Error)?.message || cause)
      } finally {
        opening = null
      }
    })()
    return opening
  }

  /** 新对话：这本书换一个会话键，旧会话留在 Agent 板块里；新会话同样等第一条消息再建 */
  const startNew = async () => {
    if (!bookId.value) return
    appSettings.setItem(`${EPOCH_PREFIX}${bookId.value}`, String(epochOf(bookId.value) + 1))
    await flushAppSettings()
    await open(true)
  }

  watch(bookId, () => void open(), { immediate: true })
  watch(() => editor.activeChapterId, () => { if (ready.value) syncAnchor() })
  watch(() => aiChat.pendingSelections.length, () => { if (ready.value) drainSelections() })
  // 面板关掉：Agent 板块里的新对话不能按作品会话的样子建
  if (getCurrentInstance()) onUnmounted(() => store.clearSessionBlueprint())

  /** 这本书的全部会话：作品会话与旧版妙笔记录，最近的在前 */
  const bookSessions = computed(() =>
    store.conversations
      .filter(item => item.references.some(r => r.kind === 'book' && r.bookId === bookId.value))
      .sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0))
  )

  return {
    store,
    ready,
    error,
    open,
    startNew,
    select: (id: string) => store.selectConversation(id),
    bookSessions,
    session: computed(() => store.active),
    hasMessages: computed(() => (store.active?.messages.length || 0) > 0),
  }
}
