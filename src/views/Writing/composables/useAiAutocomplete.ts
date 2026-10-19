import { loadCharacterContext } from '@/storage/character-context'
import { appSettings } from '@/storage/app-settings'
import { computed, ref, watch, type ComputedRef, type Ref } from 'vue'
import type { Editor } from '@tiptap/vue-3'
import { ElMessage } from 'element-plus'
import { getAiShortcutTitle } from '@/utils/platform'
import { NO_MODEL_MESSAGE, requestLocalChatCompletion, streamLocalChatCompletion } from '@/utils/local-ai-client'
import { autocompleteTemperature, buildAutocompleteMessages } from '@/config/ai-prompts'
import { editorContext } from '@/agent/writing-context'
import { useAiModelStore } from '@/stores/ai-model'
import { runDraftGuard } from '@/ai/verify/consistency'
import { buildNotesContext } from '@/storage/ai-notes'
import { buildStyleExemplars } from '@/ai/memory/style'
import {
  extractAutocompletePrefix,
  extractSceneTailAnchor,
} from '../editor-utils/content-normalize'

interface AiAutocompleteOptions {
  bookId: () => string | number | undefined
  editor: Ref<Editor | undefined>
  workflowMode: ComputedRef<boolean>
  workflowLocked: ComputedRef<boolean>
  workflowManualEditUnlocked: ComputedRef<boolean>
  chapterContentReady: Ref<boolean>
  activeChapterId: Ref<number | null>
  activeChapterTitle: Ref<string>
  activeChapterSummary: Ref<string>
  /** 正文被 AI 插入后标脏排队落盘（函数在组件里晚于本组合式声明，经闭包延迟取用） */
  onDraftDirty: () => void
  /** 有选区时快捷键改为弹出气泡菜单 */
  onSelectionMenu: () => void
  /** 流式续写每插入一段就回调一次，用于码字账本把这些字记到 AI 名下 */
  onAiTextInserted: (text: string) => void
  /** 一次快捷续写结束后回调整段文字，用于记下 AI 片段的指纹 */
  onAiSpanInserted?: (text: string) => void
}

/**
 * 打字补全（被动幽灵字）与 Cmd/Alt+/ 快捷续写（流式逐字进正文）。
 * AI 开关持久化在 localStorage；本面板模型可临时覆盖全局默认。
 * 逻辑自 WritingEditor 原样搬迁，行为不变。
 */
export const useAiAutocomplete = (options: AiAutocompleteOptions) => {
  const {
    editor,
    workflowMode,
    workflowLocked,
    workflowManualEditUnlocked,
    chapterContentReady,
    activeChapterId,
    activeChapterTitle,
    activeChapterSummary,
    onDraftDirty,
    onSelectionMenu,
    onAiTextInserted,
    onAiSpanInserted,
  } = options

  const isAiThinking = ref(false)
  const aiBusyCounter = ref(0)
  const autocompleteController = ref<AbortController | null>(null)
  // 快捷续写单独一把中止控制器：幽灵字插件在每次正文变动时都会"取消建议请求"，
  // 若与它共用一把，流式续写插进第一个字就会被自己触发的取消掐断
  const nextBeatController = ref<AbortController | null>(null)

  interface AiConfigState {
    enabled: boolean
    /** 快捷续写写完后在后台核对设定，有实质冲突时自动修订刚插入的段落 */
    verify: boolean
  }

  const defaultAiConfig: AiConfigState = {
    enabled: true,
    verify: true,
  }

  const AI_CONFIG_STORAGE_KEY = 'ew_ai_copilot_config'

  const readAiConfig = (): AiConfigState => {
    if (typeof window === 'undefined') return { ...defaultAiConfig }
    try {
      const cached = appSettings.getItem(AI_CONFIG_STORAGE_KEY)
      if (!cached) return { ...defaultAiConfig }
      const parsed = JSON.parse(cached)
      return {
        enabled:
          typeof parsed.enabled === 'boolean'
            ? parsed.enabled
            : defaultAiConfig.enabled,
        verify: typeof parsed.verify === 'boolean' ? parsed.verify : defaultAiConfig.verify,
      }
    } catch {
      return { ...defaultAiConfig }
    }
  }

  const aiConfig = ref<AiConfigState>(readAiConfig())

  // 本面板临时覆盖模型：空值 = 跟随「AI 模型」页的全局默认；选择不回写全局
  const autocompleteModelOverride = ref('')
  const aiModelStore = useAiModelStore()

  watch(
    aiConfig,
    (nextVal) => {
      if (typeof window === 'undefined') return
      try {
        appSettings.setItem(
          AI_CONFIG_STORAGE_KEY,
          JSON.stringify(nextVal),
        )
      } catch {
        // ignore storage quota issues
      }
    },
    { deep: true },
  )

  const aiShortcutTip = computed(() => getAiShortcutTitle())

  const aiStatusText = computed(() => {
    if (isAiThinking.value) return 'AI思考中...'
    return aiConfig.value.enabled ? 'AI 助手已开' : 'AI 助手已关'
  })

  // AI Copilot 获取建议
  const setAiBusy = (busy: boolean) => {
    aiBusyCounter.value = Math.max(0, aiBusyCounter.value + (busy ? 1 : -1))
    isAiThinking.value = aiBusyCounter.value > 0
  }

  const squashText = (text: string) => text.replace(/\s+/g, '')

  /**
   * 续写写完后在后台核对设定：对照角色资料与故事状态找实质冲突，有就把刚插入的那段换成修订稿。
   * 作者已经改动过这段、或切了章，则只提示不替换——核对永远不能覆盖作者的手。
   */
  const guardContinuation = async (params: {
    ed: Editor
    from: number
    to: number
    text: string
    preText: string
    chapterIdAtStart: number
    modelCode: string
  }) => {
    setAiBusy(true)
    try {
      const outcome = await runDraftGuard({
        bookId: options.bookId() ?? '',
        chapterId: params.chapterIdAtStart,
        draft: params.text,
        previousTail: params.preText,
        modelCode: params.modelCode,
      })
      if (outcome.status === 'failed') {
        console.warn('续写核对未完成', outcome.error)
        return
      }
      const substantive = outcome.issues.filter(issue => issue.severity !== 'low').length
      if (!outcome.revised) {
        if (substantive) ElMessage.info(`续写与设定可能有 ${substantive} 处出入：${outcome.issues[0].text}`)
        return
      }
      const { ed } = params
      if (Number(activeChapterId.value || 0) !== params.chapterIdAtStart || workflowMode.value || workflowLocked.value) return
      const to = Math.min(params.to, ed.state.doc.content.size)
      if (squashText(ed.state.doc.textBetween(params.from, to, '\n')) !== squashText(params.text)) {
        ElMessage.info('续写段落已被改动，设定核对结果未应用')
        return
      }
      ed.chain().insertContentAt({ from: params.from, to }, outcome.text).run()
      onDraftDirty()
      ElMessage.success(`已核对设定，修订了 ${substantive} 处：${outcome.issues[0].text}`)
    } catch (error) {
      if ((error as { name?: string })?.name !== 'AbortError') console.warn('续写核对未完成', error)
    } finally {
      setAiBusy(false)
    }
  }

  const fetchAiSuggestion = async (payload: {
    context: string
    mode: 'inline' | 'next_beat'
  }): Promise<string | null> => {
    if (
      workflowMode.value ||
      !workflowManualEditUnlocked.value ||
      !aiConfig.value.enabled
    )
      return null

    setAiBusy(true)
    autocompleteController.value?.abort()
    const controller = new AbortController()
    autocompleteController.value = controller
    try {
      const localPreText = extractAutocompletePrefix(payload.context, 3, 220)
      const sceneAnchor = extractSceneTailAnchor(localPreText, 2, 160)
      // 被动触发的补全：没配模型就静默跳过，不打断码字（设置浮层里能看到模型状态）
      let modelCode = autocompleteModelOverride.value || aiModelStore.effectiveTextModel
      if (!modelCode) {
        await aiModelStore.loadTextModels()
        modelCode = aiModelStore.effectiveTextModel
      }
      if (!modelCode) return null
      const [characterContext, notesContext, contextBlocks] = await Promise.all([
        loadCharacterContext(options.bookId(), [payload.context, activeChapterTitle.value, activeChapterSummary.value].join('\n')),
        buildNotesContext(options.bookId(), 800),
        editorContext(String(options.bookId() ?? ''), activeChapterId.value, 'ghost'),
      ])
      if (controller.signal.aborted) return null
      const data = await requestLocalChatCompletion({
        scene: 'typing_autocomplete',
        sceneLabel: '打字补全',
        modelCode,
        temperature: autocompleteTemperature(payload.mode),
        messages: buildAutocompleteMessages({
          preText: localPreText,
          characterContext: [notesContext, characterContext].filter(Boolean).join('\n\n'),
          sceneAnchor,
          chapterTitle: activeChapterTitle.value || '',
          chapterSummary: activeChapterSummary.value || '',
          contextBlocks,
          mode: payload.mode,
        }),
        maxTokens: payload.mode === 'next_beat' ? 600 : 200,
        signal: controller.signal,
      })
      // 请求期间工作流重新进入生成态时丢弃建议，避免续写补全插进生成中的正文。
      if (workflowMode.value || !workflowManualEditUnlocked.value) return null
      const suggestion = (data || '').trim()
      return suggestion || null
    } catch (error) {
      if (error?.name === 'AbortError') return null
      const message = String(error?.msg || error?.message || '')
      if (
        message.includes('频繁') ||
        message.includes('rate limit') ||
        message.includes('Too Many Requests')
      ) {
        throw error
      }
      console.warn('AI Copilot fetch failed', error)
      return null
    } finally {
      if (autocompleteController.value === controller) {
        autocompleteController.value = null
      }
      setAiBusy(false)
    }
  }

  /** 切章/卸载时统一掐掉所有在途 AI 请求（幽灵字建议 + 快捷续写） */
  const abortAiRequests = () => {
    autocompleteController.value?.abort()
    nextBeatController.value?.abort()
  }

  const cancelAiSuggestion = () => {
    if (!autocompleteController.value) return
    autocompleteController.value.abort()
  }

  /**
   * Cmd/Alt + / 快捷续写：流式逐字写进正文（不再整段干等）。
   * 位置追踪用"文档尺寸差"而不是字符数——插入内容跨段时节点边界也占位置。
   * 流入期间校验流入区未被用户改动（比较时忽略换行差异），改了就停在当前进度；
   * 切章或进入工作流锁定也会掐断，防止旧章的续写插进新章。
   */
  const triggerAiShortcut = async () => {
    if (
      !editor.value ||
      workflowMode.value ||
      !workflowManualEditUnlocked.value
    )
      return
    const ed = editor.value
    const { from, to, empty } = ed.state.selection
    if (!empty && to - from > 0) {
      onSelectionMenu()
      return
    }

    const context = ed.state.doc
      .textBetween(Math.max(0, from - 400), from, '\n', '\n')
      .trim()
    if (!context) {
      ElMessage.warning('请先输入内容或选中文本后再触发 AI')
      return
    }

    setAiBusy(true)
    // 开始续写前掐掉在途的幽灵字建议与上一次续写
    autocompleteController.value?.abort()
    nextBeatController.value?.abort()
    const controller = new AbortController()
    nextBeatController.value = controller
    const chapterIdAtStart = Number(activeChapterId.value || 0)
    const normalizeFlow = (text: string) => text.replace(/\s+/g, '')
    try {
      let modelCode = autocompleteModelOverride.value || aiModelStore.effectiveTextModel
      if (!modelCode) {
        await aiModelStore.loadTextModels()
        modelCode = aiModelStore.effectiveTextModel
      }
      if (!modelCode) {
        ElMessage.warning(NO_MODEL_MESSAGE)
        return
      }
      const localPreText = extractAutocompletePrefix(context, 3, 220)
      const sceneAnchor = extractSceneTailAnchor(localPreText, 2, 160)
      const contextBlocks = await editorContext(String(options.bookId() ?? ''), chapterIdAtStart || null, 'continue')
      if (controller.signal.aborted || Number(activeChapterId.value || 0) !== chapterIdAtStart) return

      // 备忘与作者自己的文风范例一起附上：范例按语义从前文取，没开向量检索就没有
      const [characterContext, notesContext, exemplars] = await Promise.all([
        loadCharacterContext(options.bookId(), [context, activeChapterTitle.value, activeChapterSummary.value].join('\n')),
        buildNotesContext(options.bookId()),
        buildStyleExemplars({ bookId: options.bookId(), text: localPreText, excludeChapterId: chapterIdAtStart }),
      ])
      if (controller.signal.aborted || activeChapterId.value !== chapterIdAtStart) return
      const startPos = from
      let insertPos = from
      let accumulated = ''
      let interrupted = false
      let errored = false
      const interrupt = () => {
        interrupted = true
        controller.abort()
      }

      await new Promise<void>(resolve => {
        void streamLocalChatCompletion(
          {
            scene: 'typing_autocomplete',
            sceneLabel: '快捷续写',
            modelCode,
            temperature: autocompleteTemperature('next_beat'),
            signal: controller.signal,
            messages: buildAutocompleteMessages({
              preText: localPreText,
              characterContext: [notesContext, characterContext, exemplars].filter(Boolean).join('\n\n'),
              sceneAnchor,
              chapterTitle: activeChapterTitle.value || '',
              chapterSummary: activeChapterSummary.value || '',
              contextBlocks,
              mode: 'next_beat',
            }),
          },
          {
            onDelta: text => {
              if (interrupted || !text) return
              if (
                workflowMode.value ||
                workflowLocked.value ||
                !chapterContentReady.value ||
                Number(activeChapterId.value || 0) !== chapterIdAtStart
              ) {
                interrupt()
                return
              }
              const currentFlow = ed.state.doc.textBetween(startPos, insertPos, '\n')
              if (normalizeFlow(currentFlow) !== normalizeFlow(accumulated)) {
                interrupt()
                ElMessage.info('正文有改动，续写已停在当前进度')
                return
              }
              const sizeBefore = ed.state.doc.content.size
              ed.chain().insertContentAt(insertPos, text).run()
              insertPos += ed.state.doc.content.size - sizeBefore
              accumulated += text
              onAiTextInserted(text)
            },
            onDone: () => resolve(),
            onError: message => {
              errored = true
              if (!interrupted) ElMessage.error(message)
              resolve()
            },
          }
        )
      })

      if (!accumulated && !interrupted && !errored) {
        ElMessage.warning('AI 暂无返回内容')
        return
      }
      if (accumulated) onAiSpanInserted?.(accumulated)
      if (!interrupted && accumulated && Number(activeChapterId.value || 0) === chapterIdAtStart) {
        ed.chain().focus().setTextSelection(Math.min(insertPos, ed.state.doc.content.size)).run()
        onDraftDirty()
        if (aiConfig.value.verify && !errored && accumulated.trim().length >= 80) {
          void guardContinuation({ ed, from: startPos, to: insertPos, text: accumulated, preText: localPreText, chapterIdAtStart, modelCode })
        }
      }
    } finally {
      setAiBusy(false)
      if (nextBeatController.value === controller) {
        nextBeatController.value = null
      }
    }
  }


  return {
    aiConfig,
    autocompleteModelOverride,
    aiShortcutTip,
    aiStatusText,
    isAiThinking,
    isAiStreaming: computed(() => nextBeatController.value !== null),
    abortAiRequests,
    setAiBusy,
    fetchAiSuggestion,
    cancelAiSuggestion,
    triggerAiShortcut,
  }
}
