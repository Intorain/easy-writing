import { computed, ref, watch } from 'vue'
import { defineStore } from 'pinia'
import { ElMessage, ElNotification } from 'element-plus'
import { agentCommand, listenAgent } from '@/agent/client'
import { inkConfirm } from '@/utils/ink-confirm'
import { isTauriRuntime } from '@/storage/writing-storage'
import { listLocalAiModels, getLocalAiPreference } from '@/storage/local-ai-models'
import type { AiModelOption } from '@/types/ai-model'
import type {
  AgentArtifact,
  AgentAutomationRule,
  AgentAutomationRun,
  AgentAutomationSettings,
  AgentAutomationTask,
  AgentBatchJob,
  AgentDeliveryRecord,
  AgentBreakdownJob,
  AgentDistillationJob,
  AgentConversation,
  AgentImpactReport,
  AgentIndexEvent,
  AgentJournalDay,
  AgentJournalDaySummary,
  AgentMemory,
  AgentPlatformProfile,
  AgentProject,
  AgentPromptDraft,
  AgentReference,
  AgentReferenceImport,
  AgentBreakdownResult,
  AgentRule,
  AgentRuleCategory,
  AgentRulesetPreviewResult,
  AgentScenario,
  AgentSkill,
  AgentSkillDraft,
  AgentStyleProfile,
  AgentThread,
  AgentWorkspaceSnapshot
} from '@/types/agent-creation'
import type { AuthorAnswer, PlatformProfile } from '@/types/canon'

export const useAgentWorkspace = defineStore('agent-workspace', () => {
  const projects = ref<AgentProject[]>([])
  const conversations = ref<AgentConversation[]>([])
  const artifacts = ref<AgentArtifact[]>([])
  const jobs = ref<AgentBatchJob[]>([])
  const breakdownJobs = ref<AgentBreakdownJob[]>([])
  const distillationJobs = ref<AgentDistillationJob[]>([])
  function upsertDistillation(job: AgentDistillationJob) {
    const index = distillationJobs.value.findIndex(item => item.id === job.id)
    if (index < 0) distillationJobs.value.push(job)
    else {
      const old = distillationJobs.value[index]
      if (old.revision > job.revision || (old.revision === job.revision && old.doneUnits > job.doneUnits)) return
      distillationJobs.value[index] = job
    }
  }
  const openedBreakdown = ref<AgentBreakdownResult | null>(null)
  /** 作者在进度卡上收起的已完成任务（按任务与完成时间记，重新拆完会再出现） */
  const dismissedBreakdowns = ref<string[]>([])
  const memories = ref<AgentMemory[]>([])
  const skills = ref<AgentSkill[]>([])
  const availableReferences = ref<AgentReference[]>([])
  const models = ref<AiModelOption[]>([])
  const activeId = ref<string | null>(null)
  const projectId = ref<string | null>(null)
  const openedArtifactId = ref<string | null>(null)
  const newDraft = ref('')
  const newReferences = ref<AgentReference[]>([])
  /** 妙笔预备的作品会话：作者第一次发消息时按它建（键、标题、通道档案），见 prepareBookSession / ensureSession */
  const sessionBlueprint = ref<{ requestId: string; title: string; channel: { kind: string; label: string; approval: string } } | null>(null)
  const model = ref('')
  const enterToSend = ref(true)
  const selectedSkillId = ref<string | null>(null)
  const nextScenario = ref<AgentScenario>('review')
  const loading = ref(false)
  const submitting = ref(false)
  let newRequestId = crypto.randomUUID()
  const ready = ref(false)
  const error = ref('')
  let initialization: Promise<void> | null = null
  let unlisten: (() => void) | undefined
  let draftTimer: ReturnType<typeof setTimeout> | undefined
  let draftSave = Promise.resolve()
  const dirtyArtifacts = new Set<string>()
  // 保存时撞上版本冲突的编辑（多半是 Agent 刚改过同一份成果）：之后发消息、自动存草稿不再带着它重试，
  // 免得一次冲突挡住所有发送；编辑仍留在面板里，作者手动保存时照常核对版本。
  const staleArtifacts = new Set<string>()
  const artifactSaves = new Map<string, Promise<void>>()
  const pendingDrafts = new Map<
    string,
    { draft: string; draftReferences: AgentReference[]; draftBaseMessageId: string | null; promptDraft?: AgentPromptDraft | null }
  >()
  const active = computed(() => conversations.value.find(item => item.id === activeId.value) || null)
  const openedArtifact = computed(
    () => artifacts.value.find(item => item.id === openedArtifactId.value) || null
  )
  // partial 只表示稿件未完成；是否正在写还要核对所属会话和本次执行。
  function isArtifactWriting(artifact: AgentArtifact) {
    const session = conversations.value.find(item => item.id === artifact.sessionId)
    return !!artifact.partial && session?.status === 'running' && !!session.runId &&
      (artifact.generationRunId || artifact.runId) === session.runId
  }
  const references = computed(() => active.value?.draftReferences ?? active.value?.references ?? newReferences.value)
  const modelLabel = computed(() => models.value.find(item => item.code === model.value)?.name || '默认模型')
  /** 当前会话进行中的批量连载任务 */
  const activeJob = computed(
    () =>
      jobs.value.find(
        job =>
          job.sessionId === activeId.value &&
          ['queued', 'running', 'waiting', 'paused', 'failed'].includes(job.status)
      ) || null
  )
  const breakdownKey = (job: AgentBreakdownJob) => `${job.id}:${job.finishedAt || job.createdAt || ''}`
  /** 进度只属于发起任务的对话，与当前引用了哪些书无关。 */
  const activeBreakdowns = computed(() => breakdownJobs.value.filter(job =>
    !!activeId.value && job.sessionId === activeId.value && ['running','paused','failed'].includes(job.status)))
  async function openBreakdown(result: AgentBreakdownResult) {
    try { await saveArtifact(); openedArtifactId.value = null; openedBreakdown.value = result }
    catch (error) { failure(error) }
  }
  watch(openedArtifactId, id => { if (id) openedBreakdown.value = null })
  function upsertBreakdownJob(job: AgentBreakdownJob) {
    if (!job?.id) return
    const index = breakdownJobs.value.findIndex(item => item.id === job.id)
    const current = breakdownJobs.value[index]
    if (current && (current.revision || 0) > (job.revision || 0)) return
    if (index < 0) breakdownJobs.value.unshift(job)
    else breakdownJobs.value[index] = job
    // 参考书的“已拆 N 章”跟着刷新
    const reference = availableReferences.value.find(r => r.kind === 'breakdown' && r.bookId === job.bookId)
    if (reference && job.done) reference.description = reference.description.replace(/ · 已拆 \d+ 章$/, '') + ` · 已拆 ${job.done} 章`
  }
  function dismissBreakdown(job: AgentBreakdownJob) {
    dismissedBreakdowns.value.push(breakdownKey(job))
  }
  function upsertJob(job: AgentBatchJob) {
    const index = jobs.value.findIndex(item => item.id === job.id)
    const current = jobs.value[index]
    if (current && (current.revision || 0) > (job.revision || 0)) return
    if (index < 0) jobs.value.unshift(job)
    else jobs.value[index] = job
  }
  const draftReloads = new Set<string>()
  /** 每收到一次索引事件加一，作品状态面板据此重新读取 */
  const indexTick = ref(0)
  /** 索引器没跑起来的原因（没有模型、整轮失败）；正常完成后清空 */
  const indexNotice = ref('')
  function receiveIndex(value: AgentIndexEvent) {
    if (value.lane === 'reference') {
      // 拆书通道的结果在拆书进度卡上，不占作品索引的提示
      indexTick.value++
      return
    }
    if (value.event === 'index_finished') {
      indexNotice.value =
        value.summary?.skipped === 'no_model'
          ? '还没有可用的文本模型，索引暂未运行。请先在模型管理中配置，也可以为“Agent 索引”单独指定一个便宜的模型。'
          : value.summary?.aborted === 'all_failed'
            ? '上一轮索引的章节全部提取失败，多半是模型服务暂时不可用；之后会自动重试。'
            : ''
    } else if (value.event === 'index_failed' && !value.chapterId && value.message) {
      indexNotice.value = `索引中断：${value.message}`
    }
    indexTick.value++
  }

  function failure(reason: unknown) {
    const message = String(reason instanceof Error ? reason.message : reason)
    error.value = message
    ElMessage.error(message)
  }
  function receive(session: AgentConversation) {
    const index = conversations.value.findIndex(item => item.id === session.id)
    const current = conversations.value[index]
    if (current && (current.revision || 0) > (session.revision || 0)) return
    // 后台快照不覆盖用户正在输入的草稿，避免流式回复抢写输入框。
    if (current) {
      session.draft = current.draft
      const pending = pendingDrafts.get(session.id)
      if (pending) session.draftReferences = pending.draftReferences.map(reference => ({ ...reference }))
      const oldQuestion = current.messages.find(m => m.choice?.status === 'pending')?.id
      const newQuestion = session.messages.find(m => m.choice?.status === 'pending')?.id
      if (oldQuestion && oldQuestion === newQuestion) {
        session.choiceId = current.choiceId || session.choiceId
        // 作者正在填的引导卡不被后台快照抢写
        session.promptDraft = current.promptDraft ?? session.promptDraft
      }
    }
    if (index < 0) conversations.value.unshift(session)
    else conversations.value[index] = session
  }
  async function reload() {
    const value = await agentCommand<AgentWorkspaceSnapshot>('load')
    projects.value = value.projects
    value.conversations.forEach(receive)
    conversations.value = conversations.value.filter(s => value.conversations.some(item => item.id === s.id))
    artifacts.value = value.artifacts.map(incoming => {
      const editing = artifacts.value.find(a => a.id === incoming.id)
      // 未提交的稿件保留读取版本，后续保存才能发现后台变更，而非静默覆盖。
      if (editing && dirtyArtifacts.has(incoming.id)) return editing
      return incoming
    })
    memories.value = value.memories
    skills.value = value.skills
    jobs.value = value.jobs || []
    breakdownJobs.value = value.breakdownJobs || []
    distillationJobs.value = value.distillationJobs || []
    availableReferences.value = value.references
    models.value = (await listLocalAiModels({ scene: 'text' })).data.filter(item => item.status === 1)
  }
  async function initialize() {
    if (initialization) return initialization
    loading.value = true
    initialization = (async () => {
      if (!isTauriRuntime()) {
        error.value = '请在桌面客户端使用 Agent'
        return
      }
      if (!unlisten)
        unlisten = await listenAgent(
          session => {
            receive(session)
            if (
              session.status !== 'running' ||
              session.messages.some(m => m.artifactId && !artifacts.value.some(a => a.id === m.artifactId))
            )
              void reload().catch(failure)
          },
          value => {
            const task = conversations.value.find(s => s.id === value.sessionId && s.runId === value.runId)
            const reply = task?.messages.find(m => m.id === `${value.runId}:reply`)
            if (reply) reply.text = value.text
          },
          {
            onDraft: value => {
              const session = conversations.value.find(item => item.id === value.sessionId)
              if (session?.status !== 'running' || session.runId !== value.runId) return
              // 写作模型的流式片段：成果已在列表里就直接更新进度，否则读一次工作区把它加载进来
              const artifact = artifacts.value.find(a => a.id === value.artifactId)
              if (artifact) {
                if (artifact.sessionId === value.sessionId && artifact.partial && !dirtyArtifacts.has(artifact.id)) {
                  artifact.generationRunId = value.runId
                  artifact.content = value.text
                }
                return
              }
              if (draftReloads.has(value.artifactId)) return
              draftReloads.add(value.artifactId)
              void reload()
                .catch(failure)
                .finally(() => draftReloads.delete(value.artifactId))
            },
            onDistillation: value => {
              upsertDistillation(value)
              if (value.status === 'awaiting_scope' || value.status === 'completed') void reload().catch(failure)
            },
            onBatch: value => {
              if ('queue' in value) upsertJob(value)
              else if (value.error) ElMessage.error(String(value.error))
            },
            onIndex: receiveIndex,
            onBreakdown: value => { upsertBreakdownJob(value); void reload().catch(failure) },
            onNotice: showNotice,
            onAutomation: () => { automationVersion.value += 1 }
          }
        )
      await reload()
      // 窗口不在时攒下的桌面通知：现在显示
      void agentCommand<AgentDeliveryRecord[]>('delivery_pending', { channel: 'desktop' })
        .then(records => records.forEach(showNotice))
        .catch(() => undefined)
      ready.value = true
      error.value = ''
      // 旧版妙笔的对话记录搬进会话：后台做，搬到了再刷新一次列表
      void import('@/agent/legacy-chats')
        .then(({ importAllLegacyMiaobiChats }) => importAllLegacyMiaobiChats())
        .then(outcome => { if (outcome.imported) return reload() })
        .catch(failure)
    })()
      .catch(failure)
      .finally(() => {
        loading.value = false
        initialization = null
      })
    return initialization
  }
  function queueDraft(sessionId = activeId.value) {
    const s = conversations.value.find(item => item.id === sessionId)
    if (!s) return
    pendingDrafts.set(s.id, {
      draft: s.draft,
      draftReferences: (s.draftReferences ?? s.references).map(r => ({ ...r })),
      draftBaseMessageId: [...s.messages].reverse().find(message => message.role === 'user')?.id ?? null,
      ...(s.status === 'choice'
        ? { promptDraft: s.promptDraft ? JSON.parse(JSON.stringify(s.promptDraft)) : null }
        : {})
    })
    clearTimeout(draftTimer)
    draftTimer = setTimeout(() => void flushDrafts().catch(failure), 250)
  }
  async function flushDrafts() {
    clearTimeout(draftTimer)
    const batch = new Map(pendingDrafts)
    pendingDrafts.clear()
    // 只提交待编辑字段；发送前等待，避免旧草稿请求在发送后写回。
    draftSave = draftSave.then(async () => {
      for (const [id, value] of batch) await agentCommand('update_session', { id, ...value })
    })
    try {
      await draftSave
    } catch (e) {
      draftSave = Promise.resolve()
      for (const [id, v] of batch) if (!pendingDrafts.has(id)) pendingDrafts.set(id, v)
      throw e
    }
    await saveArtifactsAlongside([...dirtyArtifacts])
  }
  /** 随发消息、存草稿顺带保存面板里的编辑；没存上的报一次错并记下，不挡住这次发送。 */
  async function saveArtifactsAlongside(ids: Array<string | null | undefined>) {
    for (const id of new Set(ids)) {
      if (!id || staleArtifacts.has(id) || !dirtyArtifacts.has(id)) continue
      try {
        await saveArtifact(id)
      } catch (e) {
        staleArtifacts.add(id)
        failure(e)
      }
    }
  }
  const draft = computed({
    get: () => active.value?.draft ?? newDraft.value,
    set: (value: string) => {
      if (active.value) {
        active.value.draft = value
        queueDraft()
      } else newDraft.value = value
    }
  })
  function newConversation(id?: string) {
    newRequestId = crypto.randomUUID()
    sessionBlueprint.value = null
    if (active.value) queueDraft()
    activeId.value = null
    selectedSkillId.value = null
    openedArtifactId.value = null
    openedBreakdown.value = null
    newDraft.value = ''
    projectId.value = id || null
    const p = projects.value.find(item => item.id === id)
    newReferences.value = p?.bookId
      ? availableReferences.value.filter(r => r.kind === 'book' && r.bookId === p.bookId)
      : []
  }
  /** 这本书的作品会话（妙笔建的），最近用过的在前；旧版妙笔记录不算 */
  function bookSessionsOf(bookId: string) {
    return conversations.value
      .filter(s => !s.legacy && s.channel?.label === '妙笔' && s.references.some(r => r.kind === 'book' && r.bookId === bookId))
      .sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0))
  }
  /**
   * 作品会话：妙笔按书用，键 `book:<bookId>`（作者开新对话时加编号），标题《书名》，通道按桌面但写操作一律先出确认卡（决策 D3）。
   * 打开面板不建会话：这本书已有作品会话就选中最近一条；没有（或作者要开新对话）就只记下"要建成什么样"，
   * 作者第一次发消息时由 ensureSession 真正建立。不聊就不留空对话。
   */
  async function prepareBookSession(book: { id: string; title: string }, epoch = 0, fresh = false) {
    if (!ready.value) await initialize()
    if (!fresh) {
      const existing = bookSessionsOf(book.id)[0]
      if (existing) {
        selectConversation(existing.id)
        return existing
      }
    }
    if (active.value) queueDraft()
    activeId.value = null
    selectedSkillId.value = null
    openedArtifactId.value = null
    openedBreakdown.value = null
    projectId.value = null
    newDraft.value = ''
    newReferences.value = [{ id: `book:${book.id}`, kind: 'book', bookId: book.id, title: book.title, description: '' }]
    sessionBlueprint.value = {
      requestId: epoch ? `book:${book.id}:${epoch}` : `book:${book.id}`,
      title: `《${book.title}》`,
      channel: { kind: 'desktop', label: '妙笔', approval: 'always' }
    }
    return null
  }
  /** 妙笔面板关掉后，Agent 板块的新对话不能按作品会话的样子建 */
  function clearSessionBlueprint() {
    sessionBlueprint.value = null
  }
  function selectConversation(id: string) {
    sessionBlueprint.value = null
    if (active.value) queueDraft()
    if (activeId.value !== id) selectedSkillId.value = null
    activeId.value = id
    projectId.value = active.value?.projectId || null
    openedArtifactId.value = null
    openedBreakdown.value = null
    model.value = active.value?.modelCode || ''
  }
  function addReference(reference: AgentReference) {
    if (!references.value.some(r => r.id === reference.id)) references.value.push({ ...reference })
    queueDraft()
  }
  function attachImportedReference(reference: AgentReference, sessionId: string | null) {
    if (!availableReferences.value.some(r => r.id === reference.id)) availableReferences.value.push(reference)
    if (activeId.value === sessionId) { addReference(reference); return }
    const target = conversations.value.find(s => s.id === sessionId)
    if (target) {
      const pending = (target.draftReferences ??= [...target.references])
      if (!pending.some(r => r.id === reference.id)) { pending.push(reference); queueDraft(target.id) }
    }
  }
  function removeReference(id: string) {
    const i = references.value.findIndex(r => r.id === id)
    if (i >= 0) references.value.splice(i, 1)
    queueDraft()
  }
  /** 输入框添加的本地资料：正文交给宿主存一份，引用只带编号、标题和字数 */
  async function addMaterial(title: string, content: string) {
    const saved = await agentCommand<{ materialId: string; title: string; chars: number }>('material_put', {
      title,
      content
    })
    addReference({
      id: `material:${saved.materialId}`,
      title: saved.title,
      description: `本地资料 · ${saved.chars.toLocaleString()} 字`,
      kind: 'material',
      bookId: '',
      materialId: saved.materialId,
      chars: saved.chars
    })
    return saved
  }
  /**
   * 整本书按章存进参考书库，并加到当前对话的引用里。只存放、不拆：
   * 拆不拆、拆多少由作者在对话里说，Agent 发起拆解前会在对话里确认用量。同一份文本再放进来时复用。
   */
  async function importReferenceBook(book: {
    title: string
    chapters: Array<{ title: string; text: string }>
    sourceHash: string
    notes: string[]
  }) {
    const imported = await agentCommand<AgentReferenceImport>('breakdown_import', {
      title: book.title,
      chapters: book.chapters.map(chapter => ({ title: chapter.title, text: chapter.text })),
      sourceHash: book.sourceHash,
      notes: book.notes
    })
    const existing = availableReferences.value.find(r => r.id === imported.bookId)
    const reference: AgentReference = existing || {
      id: imported.bookId,
      bookId: imported.bookId,
      kind: 'breakdown',
      title: imported.title,
      description: `参考书 · ${imported.chapters} 章`
    }
    if (!existing) availableReferences.value = [...availableReferences.value, reference]
    addReference(reference)
    return imported
  }
  async function startBreakdown(projectId: string, from: number, to: number) {
    const job = await agentCommand<AgentBreakdownJob>('breakdown_start', {
      projectId,
      from,
      to,
      sessionId: activeId.value || ''
    })
    upsertBreakdownJob(job)
    return job
  }
  async function controlBreakdown(projectId: string, control: 'pause' | 'resume' | 'cancel' | 'auto_off') {
    upsertBreakdownJob(await agentCommand<AgentBreakdownJob>('breakdown_control', { projectId, control }))
  }
  async function createProject(title: string, requestId = crypto.randomUUID()) {
    const p = await agentCommand<AgentProject>('create_project', { title, requestId })
    await reload()
    return p
  }
  async function updateProject(id: string, changes: { title?: string; bookId?: string | null }) {
    await agentCommand('update_project', { id, ...changes })
    await reload()
  }
  /** 项目软删除；其中的对话由宿主移到独立会话，不跟着删除。 */
  async function deleteProject(id: string) {
    await flushDrafts()
    await agentCommand('delete_project', { id })
    if (projectId.value === id) projectId.value = null
    await reload()
  }
  async function moveConversation(id: string, destination: string | null) {
    await flushDrafts()
    receive(await agentCommand<AgentConversation>('move_session', { id, projectId: destination }))
    if (activeId.value === id) projectId.value = destination
  }
  async function ensureSession() {
    if (active.value) return active.value
    // null 是独立会话，不按引用的书名自动创建或匹配项目。
    const pid = projectId.value
    const blueprint = sessionBlueprint.value
    const requestId = blueprint?.requestId || newRequestId
    const initialDraft = newDraft.value
    const initialReferences = newReferences.value.map(r => ({ ...r }))
    const s = await agentCommand<AgentConversation>('create_session', {
      projectId: pid,
      requestId,
      title: blueprint?.title || initialDraft.trim().slice(0, 24) || '新对话',
      draft: initialDraft,
      references: initialReferences,
      ...(blueprint ? { channel: { ...blueprint.channel } } : {})
    })
    s.references = initialReferences
    s.draftReferences = initialReferences.map(reference => ({ ...reference }))
    s.draft = initialDraft
    if (!conversations.value.some(item => item.id === s.id)) conversations.value.unshift(s)
    // 建会话请求返回时，用户可能已切换页面或另开新对话，不能把焦点和草稿抢回来。
    const stillWanted = blueprint ? sessionBlueprint.value === blueprint : newRequestId === requestId
    if (activeId.value === null && stillWanted) {
      sessionBlueprint.value = null
      s.draft = newDraft.value
      activeId.value = s.id
      projectId.value = pid
      newDraft.value = ''
      newReferences.value = []
    }
    return s
  }
  function consumeSentReferences(sessionId: string, sent: AgentReference[]) {
    const keep = (reference: AgentReference) => reference.kind === 'book' || reference.pinned || !sent.some(item => item.id === reference.id)
    const session = conversations.value.find(item => item.id === sessionId)
    if (session) session.draftReferences = (session.draftReferences ?? session.references).filter(keep)
    const pending = pendingDrafts.get(sessionId)
    if (pending) pending.draftReferences = pending.draftReferences.filter(keep)
  }
  async function send() {
    if (!draft.value.trim() || submitting.value) return
    const messageSkillId = selectedSkillId.value
    const text = draft.value.trim()
    const messageReferences = references.value.map(reference => ({ ...reference }))
    const messageModelCode =
      model.value || getLocalAiPreference('agent_planner') || getLocalAiPreference('text_assist')
    const clearMessageSkill = (sessionId: string) => {
      if (activeId.value === sessionId && selectedSkillId.value === messageSkillId)
        selectedSkillId.value = null
    }
    submitting.value = true
    try {
      // 在第一次等待前绑定会话和消息快照，切换会话不能把旧技能和文字发到新会话。
      const s = await ensureSession()
      await flushDrafts()
      await saveArtifactsAlongside([openedArtifactId.value])
      if (s.status === 'running') {
        const queued = await agentCommand<AgentConversation>('queue_message', { id: s.id, text, references: messageReferences, selectedSkillId: messageSkillId })
        receive(queued)
        consumeSentReferences(s.id, messageReferences)
        const latest = conversations.value.find(item => item.id === s.id)
        if (latest?.draft.trim() === text) latest.draft = ''
        clearMessageSkill(s.id)
        return
      }
      await agentCommand('run', {
        sessionId: s.id,
        inputKind: 'message',
        text,
        references: messageReferences,
        modelCode: messageModelCode,
        selectedSkillId: messageSkillId
      })
      consumeSentReferences(s.id, messageReferences)
      clearMessageSkill(s.id)
      const latest = conversations.value.find(item => item.id === s.id)
      if (latest?.draft.trim() === text) {
        latest.draft = ''
        const pending = pendingDrafts.get(s.id)
        if (pending) pending.draft = ''
      }
      await reload()
    } catch (e) {
      failure(e)
    } finally {
      submitting.value = false
    }
  }
  async function selectDirection(artifactId: string, index: number) {
    const s = active.value
    const artifact = artifacts.value.find(item => item.id === artifactId)
    if (!s || !artifact || submitting.value || s.status === 'running') return
    const messageReferences = (s.draftReferences ?? s.references).map(reference => ({ ...reference }))
    const messageSkillId = selectedSkillId.value
    const messageModelCode = model.value || s.modelCode || getLocalAiPreference('agent_planner') || getLocalAiPreference('text_assist')
    submitting.value = true
    try {
      await flushDrafts()
      await saveArtifact(artifactId)
      const saved = artifacts.value.find(item => item.id === artifactId)
      if (!saved || JSON.stringify(saved.selectionCards?.[index]) !== JSON.stringify(artifact.selectionCards?.[index]))
        throw new Error('选题方向已变化，请重新选择')
      await agentCommand('run', {
        sessionId: s.id,
        inputKind: 'message',
        selection: { artifactId, revision: saved.revision, index },
        references: messageReferences,
        modelCode: messageModelCode,
        selectedSkillId: messageSkillId
      })
      consumeSentReferences(s.id, messageReferences)
      await reload()
      if (activeId.value === s.id) {
        if (openedArtifactId.value === artifactId) openedArtifactId.value = null
        if (selectedSkillId.value === messageSkillId) selectedSkillId.value = null
      }
    } catch (error) {
      failure(error)
    } finally {
      submitting.value = false
    }
  }
  async function pause() {
    if (active.value)
      try {
        await agentCommand('stop', { id: active.value.id })
      } catch (e) {
        failure(e)
      }
  }
  async function resume(text = '') {
    if (!active.value) return
    try {
      await agentCommand('run', {
        sessionId: active.value.id,
        inputKind: 'continue',
        text,
        resume: true,
        modelCode:
          model.value ||
          active.value.modelCode ||
          getLocalAiPreference('agent_planner') ||
          getLocalAiPreference('text_assist'),
        choiceId: active.value.choiceId
      })
      await reload()
    } catch (e) {
      failure(e)
    }
  }
  async function retryMemory() {
    if (!active.value || active.value.status !== 'completed' || submitting.value) return
    submitting.value = true
    try {
      await flushDrafts()
      await agentCommand('retry_memory', { sessionId: active.value.id })
      await reload()
    } catch (error) {
      failure(error)
    } finally {
      submitting.value = false
    }
  }
  async function choose() {
    const choice = active.value?.messages.find(m => m.choice?.status === 'pending')?.choice
    const selected = choice?.options?.find(c => c.id === active.value?.choiceId)
    if (selected) await answerPrompt({ selectedIds: [selected.id] })
  }
  /** 引导卡作答中的内容：选中项、补充文字、表单值 */
  function updatePromptDraft(value: AgentPromptDraft) {
    const s = active.value
    if (!s || s.status !== 'choice') return
    s.promptDraft = value
    if (value.selectedIds.length === 1) s.choiceId = value.selectedIds[0]
    queueDraft()
  }
  /**
   * 提交引导卡的结构化答案；宿主校验选项与必填项，校验不过时保留作者已填的内容。
   * 同时有几张卡待答时带上 messageId：宿主先记下这张的回答，全部答完才恢复运行。
   */
  async function answerPrompt(answer: AuthorAnswer, messageId?: string) {
    const s = active.value
    if (!s || submitting.value) return
    submitting.value = true
    try {
      await flushDrafts()
      await agentCommand('run', {
        sessionId: s.id,
        inputKind: 'answer',
        text: '',
        resume: true,
        answer,
        messageId,
        choiceId: answer.selectedIds[0] ?? null,
        modelCode:
          model.value ||
          s.modelCode ||
          getLocalAiPreference('agent_planner') ||
          getLocalAiPreference('text_assist')
      })
      await reload()
    } catch (e) {
      failure(e)
    } finally {
      submitting.value = false
    }
  }
  async function previewRewrite() {
    draft.value = '根据刚才的建议生成修改预览，先不要保存到正式正文。'
    await send()
  }
  async function openResourceArtifact(kind: 'chapter' | 'outline', bookTitle?: string) {
    const subject = bookTitle ? `作品《${bookTitle}》` : '当前引用作品'
    draft.value =
      kind === 'chapter'
        ? `读取${subject}的章节目录，询问我要打开哪章，再把那一章打开成稿子供我编辑。`
        : `读取并整理${subject}的大纲与人物资料，保存下来。`
    await send()
  }
  function openReport() {
    const a = artifacts.value.find(a => a.sessionId === activeId.value)
    if (a) openedArtifactId.value = a.id
  }
  async function applyArtifact(id = openedArtifactId.value) {
    const artifact = artifacts.value.find(item => item.id === id)
    if (!artifact) return
    // 采用绑定点击时看到的成果，不能在异步保存后取另一个刚切换的成果。
    await saveArtifact(artifact.id)
    // 章节候选比正文旧（候选生成后这一章改过）：先把事实摆出来，作者点了"覆盖"才按现在的版本写入
    let overwrite = false
    if (artifact.kind === 'chapter') {
      const basis = await agentCommand<{ overwritesEdits: boolean; currentWords?: number }>('apply_artifact_basis', { id: artifact.id })
      if (basis.overwritesEdits) {
        try {
          await inkConfirm(
            `这一章在稿子生成后改过${basis.currentWords ? `（现在约 ${basis.currentWords} 字）` : ''}，保存会覆盖这些改动，原正文会留一份快照。`,
            '保存并覆盖',
            { confirmButtonText: '覆盖', type: 'warning' }
          )
        } catch {
          return
        }
        overwrite = true
      }
    }
    try {
      await agentCommand(
        artifact.kind === 'skill'
          ? 'skill_adopt'
          : artifact.kind === 'book_plan'
            ? 'adopt_book_plan'
            : 'apply_artifact',
        {
          id: artifact.id,
          revision: artifact.revision || 0,
          overwrite
        }
      )
    } finally {
      // 保存被质量门拒绝时，宿主已经刷新了当前稿件的报告。
      await reload()
    }
  }
  async function undoArtifact(id = openedArtifactId.value) {
    const artifact = artifacts.value.find(item => item.id === id)
    if (artifact) {
      await agentCommand('undo_artifact', {
        id: artifact.id,
        revision: artifact.revision || 0
      })
      await reload()
    }
  }
  function markArtifactDirty() {
    if (openedArtifact.value) dirtyArtifacts.add(openedArtifact.value.id)
  }
  async function saveArtifact(id = openedArtifactId.value): Promise<void> {
    if (!id) return
    const running = artifactSaves.get(id)
    if (running) {
      await running
      return saveArtifact(id)
    }
    const artifact = artifacts.value.find(item => item.id === id)
    if (!artifact || artifact.applied || !dirtyArtifacts.has(artifact.id)) return
    const content = artifact.content
    const materialFields = artifact.materialFields
      ? JSON.parse(JSON.stringify(artifact.materialFields))
      : undefined
    const bookPlan = artifact.bookPlan ? JSON.parse(JSON.stringify(artifact.bookPlan)) : undefined
    const skillDraft = artifact.skillDraft ? JSON.parse(JSON.stringify(artifact.skillDraft)) : undefined
    const saving = (async () => {
      const saved = await agentCommand<AgentArtifact>('save_artifact', {
        id: artifact.id,
        content,
        materialFields,
        bookPlan,
        skillDraft,
        revision: artifact.revision || 0
      })
      artifact.revision = saved.revision
      if (
        artifact.content === content &&
        JSON.stringify(artifact.materialFields) === JSON.stringify(materialFields) &&
        JSON.stringify(artifact.bookPlan) === JSON.stringify(bookPlan) &&
        JSON.stringify(artifact.skillDraft) === JSON.stringify(skillDraft)
      ) {
        artifact.content = saved.content
        artifact.title = saved.title
        if (artifact.kind === 'book_plan') artifact.progress = saved.progress
        dirtyArtifacts.delete(artifact.id)
        staleArtifacts.delete(artifact.id)
      }
    })()
    artifactSaves.set(id, saving)
    try {
      await saving
    } finally {
      artifactSaves.delete(id)
    }
  }
  async function renameConversation(id: string, title: string) {
    receive(await agentCommand('update_session', { id, title }))
  }
  async function pinConversation(id: string, pinned: boolean) {
    receive(await agentCommand('update_session', { id, pinned }))
  }
  /** 忘掉这次会话：宿主归档它派生的记忆、清检查点与短期状态；对话记录与成果留着 */
  async function forgetConversation(id: string) {
    const outcome = await agentCommand<{ forgottenMemories: number }>('forget_session', { id })
    pendingDrafts.delete(id)
    await reload()
    return outcome
  }
  /** 工作备注页：有过活动的日期、某一天的工作、删一条备注 */
  /** 自动任务跑完一次就加一：自动任务页据此刷新 */
  const automationVersion = ref(0)
  const shownNotices = new Set<string>()
  /** 屏幕上同时最多几条；多出来的合成一条"另有 N 条"，全文在自动任务页的通知里 */
  const NOTICE_VISIBLE_MAX = 3
  const NOTICE_DURATION_MS = 10_000
  let visibleNotices = 0
  let overflowCount = 0
  let overflowHandle: { close: () => void } | null = null
  /** 宿主投递队列给桌面的通知：显示一次就回执，事件与启动补取都经这里，不重复显示 */
  function showNotice(record: AgentDeliveryRecord) {
    if (!record?.id || shownNotices.has(record.id)) return
    shownNotices.add(record.id)
    void agentCommand('delivery_ack', { id: record.id }).catch(() => undefined)
    if (visibleNotices < NOTICE_VISIBLE_MAX) {
      visibleNotices += 1
      ElNotification({
        title: '易创助理',
        message: record.text,
        duration: NOTICE_DURATION_MS,
        position: 'bottom-right',
        onClose: () => { visibleNotices = Math.max(0, visibleNotices - 1) }
      })
      return
    }
    overflowCount += 1
    overflowHandle?.close()
    overflowHandle = ElNotification({
      title: '易创助理',
      message: `另有 ${overflowCount} 条提醒，在自动任务页的通知里查看。`,
      duration: NOTICE_DURATION_MS,
      position: 'bottom-right',
      onClose: () => { overflowCount = 0; overflowHandle = null }
    })
  }
  async function automationList() {
    return agentCommand<AgentAutomationTask[]>('automation_list')
  }
  async function automationUpdate(id: string, patch: { enabled?: boolean; deliver?: string; activeHours?: [number, number] }) {
    return agentCommand<AgentAutomationTask>('automation_update', { id, ...patch })
  }
  async function automationDelete(id: string) {
    await agentCommand('automation_delete', { id })
  }
  async function automationRunNow(id: string) {
    await agentCommand('automation_run_now', { id })
  }
  async function automationRuns(id: string) {
    return agentCommand<AgentAutomationRun[]>('automation_runs', { id })
  }
  async function automationRules() {
    return agentCommand<AgentAutomationRule[]>('automation_rules')
  }
  async function automationRuleUpdate(event: string, patch: { enabled?: boolean; deliver?: string; params?: Record<string, number> }) {
    return agentCommand<AgentAutomationRule>('automation_rule_update', { event, ...patch })
  }
  async function automationSettings() {
    return agentCommand<AgentAutomationSettings>('automation_settings')
  }
  async function automationSettingsUpdate(patch: { activeHours?: [number, number] }) {
    return agentCommand<AgentAutomationSettings>('automation_settings_update', patch)
  }
  async function automationCheckNow() {
    return agentCommand<{ alerts?: number; skipped?: boolean }>('automation_check_now')
  }
  async function deliveryRecent() {
    return agentCommand<AgentDeliveryRecord[]>('delivery_recent')
  }
  async function deliveryFlush() {
    return agentCommand<{ sent: number }>('delivery_flush')
  }
  async function journalDays(limit = 60) {
    return agentCommand<AgentJournalDaySummary[]>('journal_days', { limit })
  }
  async function journalDay(date: string, bookId = '') {
    return agentCommand<AgentJournalDay>('journal_day', { date, bookId })
  }
  async function deleteJournalNote(id: string) {
    await agentCommand('journal_delete', { id })
  }
  async function deleteConversation(id: string) {
    await agentCommand('delete_session', { id })
    pendingDrafts.delete(id)
    conversations.value = conversations.value.filter(s => s.id !== id)
    if (activeId.value === id) newConversation()
  }
  async function saveMemory(value: AgentMemory) {
    const saved = await agentCommand<AgentMemory>('save_memory', value)
    await reload()
    return saved
  }
  async function saveSkill(value: AgentSkillDraft) {
    const saved = await agentCommand<AgentSkill>('skill_save', value)
    await reload()
    return saved
  }
  async function setSkillEnabled(id: string, enabled: boolean) {
    await agentCommand('set_skill_enabled', { id, enabled })
    await reload()
  }
  async function deleteMemory(id: string) {
    await agentCommand('delete_memory', { id })
    await reload()
  }
  async function deleteSkill(id: string) {
    await agentCommand('delete_skill', { id })
    await reload()
  }
  async function resetSkill(id: string) {
    const saved = await agentCommand<AgentSkill>('reset_skill', { id })
    await reload()
    return saved
  }
  /** 批量连载：作者在界面上选章、审阅策略与模式；直接写入模式由这一次操作授权。批量短篇按选题逐篇写 */
  /** 作者确认规则汇编：删去其中列出的重复规则与否决约束 */
  async function applyDigest(bookId: string) {
    const result = await agentCommand<{ removed: number }>('apply_digest', { bookId })
    await reload()
    return result
  }
  async function createBatch(
    args:
      | {
          bookId: string
          chapters: Array<{ chapterId: string }>
          reviewPolicy: AgentBatchJob['reviewPolicy']
          mode: 'candidate' | 'write'
        }
      | { mode: 'short'; topics: string[]; profileId: string; reviewPolicy: AgentBatchJob['reviewPolicy'] }
  ) {
    const session = await ensureSession()
    const job = await agentCommand<AgentBatchJob>('batch_create', {
      sessionId: session.id,
      modelCode: model.value || getLocalAiPreference('agent_planner') || getLocalAiPreference('text_assist'),
      skillId: selectedSkillId.value,
      ...args
    })
    upsertJob(job)
    return job
  }
  async function controlBatch(id: string, control: 'pause' | 'resume' | 'cancel') {
    const job = await agentCommand<AgentBatchJob>('batch_control', { id, control })
    upsertJob(job)
    if (control === 'cancel') await reload()
    return job
  }
  /** 疑点裁决：接受或驳回，写入作者命名空间；同一疑点不再重复提示 */
  async function saveVerdict(
    bookId: string,
    issueKey: string,
    decision: 'accepted' | 'dismissed',
    note = ''
  ) {
    await agentCommand('save_verdict', { bookId, issueKey, decision, note })
  }
  async function canonQuery<T = unknown>(view: string, bookId: string, extra: object = {}) {
    return agentCommand<T>('canon_query', { view, bookId, ...extra })
  }
  async function indexNow() {
    await agentCommand('index_now')
  }
  /** 为从未进过索引的章节补事件（onlyMissing），或全书重新提取 */
  async function indexRebuild(bookId: string, onlyMissing: boolean) {
    const result = await agentCommand<{ queued: number }>('index_rebuild', { bookId, onlyMissing })
    indexTick.value++
    return result
  }
  async function setIndexPaused(bookId: string, paused: boolean) {
    await agentCommand('index_control', { bookId, paused })
    indexTick.value++
  }
  async function overrideThread(bookId: string, threadId: string, status: AgentThread['status'], note = '') {
    await agentCommand('override_thread', { bookId, threadId, status, note })
  }
  async function impactReport(bookId: string, chapterId: string) {
    return agentCommand<AgentImpactReport>('impact_report', { bookId, chapterId })
  }
  async function saveRule(rule: {
    scope: AgentRule['scope']
    bookId: string
    category: AgentRuleCategory
    text: string
  }) {
    return agentCommand<AgentRule>('save_rule', rule)
  }
  async function deleteRule(id: string) {
    await agentCommand('delete_rule', { id })
  }
  async function savePlatformProfile(profile: PlatformProfile) {
    return agentCommand<AgentPlatformProfile>('save_platform_profile', { profile })
  }
  async function setBookProfile(bookId: string, profileId: string) {
    await agentCommand('set_book_profile', { bookId, profileId })
  }
  /** 不依附会话的规则集预览：只看作品的规则、画像与写作方法技能，不写入 */
  async function rulesetPreview(bookId: string, skillId = '') {
    return agentCommand<AgentRulesetPreviewResult>('ruleset_preview', { bookId, skillId })
  }
  async function confirmSamples(bookId: string, chapterIds: string[]) {
    return agentCommand<{ style: AgentStyleProfile }>('confirm_samples', { bookId, chapterIds })
  }
  async function lockStyle(bookId: string, points: string) {
    return agentCommand<AgentStyleProfile>('style_lock', { bookId, points })
  }
  async function rebuildStyle(bookId: string) {
    return agentCommand<AgentStyleProfile>('style_rebuild', { bookId })
  }
  return {
    projects,
    conversations,
    artifacts,
    isArtifactWriting,
    memories,
    skills,
    availableReferences,
    models,
    modelLabel,
    activeId,
    projectId,
    active,
    openedArtifactId,
    openedArtifact,
    newDraft,
    newReferences,
    draft,
    references,
    model,
    enterToSend,
    selectedSkillId,
    nextScenario,
    loading,
    submitting,
    ready,
    error,
    initialize,
    reload,
    flushDrafts,
    newConversation,
    selectConversation,
    prepareBookSession,
    clearSessionBlueprint,
    addReference,
    attachImportedReference,
    removeReference,
    createProject,
    updateProject,
    deleteProject,
    moveConversation,
    send,
    selectDirection,
    pause,
    resume,
    retryMemory,
    choose,
    answerPrompt,
    updatePromptDraft,
    previewRewrite,
    openResourceArtifact,
    openReport,
    applyArtifact,
    undoArtifact,
    saveArtifact,
    markArtifactDirty,
    renameConversation,
    pinConversation,
    deleteConversation,
    forgetConversation,
    journalDays,
    journalDay,
    deleteJournalNote,
    automationVersion,
    automationList,
    automationUpdate,
    automationDelete,
    automationRunNow,
    automationRuns,
    automationRules,
    automationRuleUpdate,
    automationSettings,
    automationSettingsUpdate,
    automationCheckNow,
    deliveryRecent,
    deliveryFlush,
    saveMemory,
    saveSkill,
    setSkillEnabled,
    deleteMemory,
    deleteSkill,
    resetSkill,
    jobs,
    activeJob,
    breakdownJobs,
    distillationJobs,
    upsertDistillation,
    openedBreakdown,
    openBreakdown,
    activeBreakdowns,
    dismissBreakdown,
    addMaterial,
    importReferenceBook,
    startBreakdown,
    controlBreakdown,
    createBatch,
    applyDigest,
    controlBatch,
    saveVerdict,
    canonQuery,
    indexNow,
    indexTick,
    indexNotice,
    indexRebuild,
    setIndexPaused,
    overrideThread,
    impactReport,
    saveRule,
    deleteRule,
    savePlatformProfile,
    setBookProfile,
    rulesetPreview,
    confirmSamples,
    lockStyle,
    rebuildStyle
  }
})
