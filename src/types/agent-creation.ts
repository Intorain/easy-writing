/** Agent 页面与桌面宿主契约。项目身份与书籍身份分开，正文只有平台作品库一份。 */
export type AgentView = 'chat' | 'memory' | 'skills' | 'automation'
export type AgentScenario = 'review' | 'rewrite' | 'create' | 'breakdown' | 'rank'
export type AgentTaskStatus = 'running' | 'paused' | 'choice' | 'completed' | 'failed'
/** breakdown：拆书库里的参考书，编号带 breakdown: 前缀，只借方法不进作者作品 */
/**
 * 引用类型：作品、卷、章节、单个人物或设定（entity）、编辑器里选中的原文（selection）、
 * 拆书项目、榜单快照（rank，不属于任何作品）、本地文本资料
 */
export type AgentReferenceKind =
  'book' | 'volume' | 'chapter' | 'entity' | 'selection' | 'material' | 'breakdown' | 'rank'

export interface AgentReference {
  id: string
  title: string
  description: string
  kind: AgentReferenceKind
  bookId: string
  chapterId?: string
  volumeId?: string
  entityType?: 'character' | 'world_setting'
  entityId?: string
  sourceId?: number
  statDate?: string
  /** 旧版附件把正文直接放在引用里；新版只带 materialId，正文由宿主保存一份 */
  content?: string
  materialId?: string
  chars?: number
  /** 跟着会话一直带着（妙笔的当前章锚点）；没标的附件发送成功即从输入框撤下，作品引用总是保留 */
  pinned?: boolean
}

export interface AgentProject {
  id: string
  title: string
  genre: string
  chapters: number
  bookId?: string | null
}

export interface AgentChoice {
  id: string
  title: string
  description: string
}

export interface AgentBreakdownResult {
  projectId: string
  title: string
  from: number
  to: number
  createdAt: number
  status: AgentBreakdownJob['status']
}

export interface AgentMessage {
  id: string
  role: 'user' | 'assistant'
  text: string
  runId?: string
  /** batch：批量连载自动发出的步骤指令，不是作者手打的 */
  /** batch：批量连载的步骤；breakdown：拆书拆完后宿主替作者接着发的一轮 */
  source?: 'batch' | 'breakdown'
  /** 回复创建与本轮结束的真实时间；旧消息没有这些字段时不推算用时。 */
  createdAt?: number
  finishedAt?: number
  distillationId?: string
  kind?: 'activity' | 'choice' | 'result'
  activity?: {
    status: 'running' | 'paused' | 'completed' | 'stopped'
    step: number
    steps: string[]
  }
  choice?: {
    status: 'pending' | 'selected' | 'skipped' | 'superseded'
    supersededBy?: { kind: 'distillation'; id: string }
    skillReview?: { id: string; hash: string; action: 'run' | 'uninstall' }
    selectedId?: string | null
    /** 多选卡的全部选中项 */
    selectedIds?: string[]
    kind?: AuthorPrompt['kind']
    requestId?: string
    options?: AgentChoice[]
    /** 这张卡的默认选中项（宿主按卡片算好） */
    defaultId?: string
    /** 这张卡的回答文字；几张卡一起答时作者消息是合并的 */
    answerText?: string
  }
  /** 引导卡原文：单选、多选、表单、并排对比、确认 */
  prompt?: AuthorPrompt
  /** 作者提交的结构化答案；selected 是宿主补上的选项标题 */
  answer?: AuthorAnswer & {
    selected?: string[]
    blank?: string[]
    /** 分步问答逐问的回答（宿主整理后的） */
    answers?: Array<{ key: string; question: string; selected: string[]; text?: string }>
  }
  references?: AgentReference[]
  /** 这条作者消息显式选择的技能快照，不继承到新消息。 */
  selectedSkill?: { id: string; name: string } | null
  bullets?: Array<{ title: string; text: string }>
  artifactId?: string
  breakdown?: AgentBreakdownResult
  /** 本轮回复的模型用量（宿主在本轮结束时记在回复消息上） */
  usage?: { input_tokens: number; output_tokens: number; total_tokens: number }
}

import type {
  AuthorAnswer,
  AuthorPrompt,
  JudgmentEvidence,
  PlatformProfile,
  QualityReport,
  DeliveryStatus,
  BookPlan,
  RankEvidence,
  SelectionCard
} from '@/types/canon'

/** 引导卡里作者选到一半、填到一半的内容；存在会话上，退出重开后还在 */
export interface AgentPromptDraft {
  /** 草稿属于哪张卡：同时有几张卡待答时互不串 */
  messageId?: string
  selectedIds: string[]
  text: string
  fields: Record<string, string>
  /** 分步问答：当前第几问、各问的回答 */
  step?: number
  steps?: Record<string, { selectedIds: string[]; text: string }>
}

/** 宿主附在契约报告上的状态字段 */
export type AgentArtifactReport = QualityReport & {
  blocked?: boolean
  stale?: boolean
}

/**
 * 宿主对选题卡证据的核对结果。数据证据的展示文字由宿主从快照读出（verified），
 * 快照或书名对不上时为 unverified；模型写的归纳都是 judgment，不作核对。
 */
export interface AgentEvidenceCheck {
  status: 'verified' | 'unverified' | 'judgment'
  /** 宿主从快照生成的展示文字，不含模型的转述 */
  text?: string
  issue?: string
  snapshot?: { sourceId: number; statDate: string; pageTitle?: string | null; itemCount?: number }
  books?: Array<{
    title: string
    rankNo?: number | null
    metricName?: string | null
    metricText?: string | null
    metricValue?: number | null
  }>
}
export type AgentSelectionCard = Omit<SelectionCard, 'evidence'> & {
  evidence: Array<(RankEvidence | JudgmentEvidence) & { check?: AgentEvidenceCheck }>
}

export interface AgentBatchJob {
  id: string
  sessionId: string
  bookId: string
  bookTitle?: string
  /** short：按选题批量写短篇，不挂作品 */
  mode: 'candidate' | 'write' | 'short'
  profileId?: string
  reviewPolicy: 'each' | 'every5' | 'onIssue'
  status: 'queued' | 'running' | 'waiting' | 'paused' | 'completed' | 'cancelled' | 'failed'
  waitingReason?: 'question' | 'review' | 'issue' | 'error' | null
  cursor: number
  queue: Array<{
    index: number
    chapterId: string
    title: string
    topic?: string
    status: 'pending' | 'running' | 'done' | 'review' | 'blocked' | 'failed' | 'skipped' | 'paused'
    artifactId?: string | null
    blocked?: boolean
    suspects?: number
    applied?: boolean
    error?: string | null
  }>
  usage?: { input_tokens: number; output_tokens: number; total_tokens: number }
  revision?: number
  updatedAt?: number
}

export interface AgentArtifact {
  delivery?: DeliveryStatus
  id: string
  title: string
  bookTitle: string
  kind:
    | 'chapter'
    | 'report'
    | 'outline'
    | 'material'
    | 'book_plan'
    | 'draft'
    | 'beats'
    | 'selection'
    | 'comparison'
    | 'structure'
    | 'short'
    | 'names'
    | 'intro'
    | 'cover'
    | 'skill'
    | 'breakdown_report'
  skillDraft?: AgentSkillDraft
  skillCopy?: boolean
  preservedFiles?: string[]
  installedSkillId?: string
  /** 封面方案：风格与作者在封面工具里生成后的地址 */
  coverStyle?: string
  coverUrl?: string
  /** 取名候选与平台核对结论 */
  nameKind?: string
  nameCheckSummary?: string
  candidates?: Array<{
    name: string
    meaning: string
    flags: string[]
    readerPromise?: string
    storyBasis?: string
    tradeoff?: string
  }>
  /** 起草时“Agent 写作”角色用的模型，评测按它分组 */
  writerModel?: { id: number; name: string } | null
  /** 短篇稿：来自哪张结构卡、这一稿的写法名、按哪个平台画像体检 */
  planArtifactId?: string
  /** 短篇为写法名；章节候选为起草时的写作方法（A/B 与评测按它分组） */
  variant?: string
  profileId?: string
  /** 仿书对照方案：逐维度的对标做法、我的做法与差异 */
  rows?: AgentComparisonRow[]
  projectIds?: string[]
  /** 拆书报告关联的参考书，不是作者作品。 */
  projectId?: string
  bookPlan?: AgentBookPlan
  /** 宿主按开书准备内容计算；前端不自行判断是否允许建书。 */
  progress?: AgentBookPlanProgress
  /** 作者实际点选的故事快照，与成果版本绑定。 */
  selectedDirection?: { artifactId: string; revision: number; index: number; card: AgentSelectionCard }
  /** 选题卡（只读），证据带宿主核对结果 */
  selectionCards?: AgentSelectionCard[]
  /** 正文尚未完整生成（包含已暂停的片段）；不能直接应用 */
  partial?: boolean
  /** 当前续写所属执行；runId 保留稿件最初创建的轮次 */
  generationRunId?: string
  /** 体检报告列表，最后一份对应当前正文；被作者编辑过的旧报告带 stale */
  reports?: AgentArtifactReport[]
  plan?: Record<string, unknown>
  rulesetId?: string
  runId?: string
  textFingerprint?: string
  revision?: number
  materialKind?: 'character' | 'outline' | 'world_setting'
  materialFields?: Record<string, string | number | string[]>
  entityId?: number
  original: string
  content: string
  applied: boolean
  sessionId?: string
  bookId?: string
  chapterId?: string
  expectedVersion?: number
  appliedVersion?: number
}

export interface AgentComparisonRow {
  dimension: string
  benchmark: string
  mine: string
  difference: string
}

export type AgentBookPlan = BookPlan

export interface AgentBookPlanProgress {
  steps: Array<{
    key: string
    label: string
    status: 'done' | 'proposed' | 'assumed' | 'partial' | 'todo'
    missing: string[]
  }>
  current: string | null
  buildable: boolean
  blocking: string[]
}

export interface AgentConversation {
  id: string
  /** 旧版妙笔搬进来的对话记录：只能查看，不能在上面继续运行 */
  legacy?: 'miaobi'
  /** 对话从哪个入口来：微信会话带 weixin，桌面与妙笔不带或为 desktop */
  channel?: { kind: string; label?: string }
  /** 引用的作品已进回收站：对话还能看，不能再往那本书里写（宿主读取时标出，不落盘） */
  bookDeleted?: boolean
  /** 作者选了"忘掉这次会话"：派生记忆已归档、检查点已清，对话记录保留 */
  forgottenAt?: number | null
  pinnedAt?: number | null
  projectId: string | null
  targetBookId?: string | null
  contextBookId?: string | null
  title: string
  scenario: AgentScenario
  status: AgentTaskStatus
  step: number
  draft: string
  references: AgentReference[]
  /** 输入框待发送引用，独立于运行中会话的资料权限。 */
  draftReferences?: AgentReference[]
  messages: AgentMessage[]
  artifactId?: string
  choiceId: string
  revision?: number
  runId?: string
  currentTool?: string | null
  currentToolProgress?: { completed: number; total: number } | null
  modelCode?: string
  error?: string | null
  taskOutcome?: {
    state: 'no_prose' | 'incomplete' | 'awaiting_author' | 'delivered'
    message: string
    artifacts: Array<{ artifactId: string; revision: number; applied: boolean; ready: boolean; deliveryStatus?: string | null }>
  }
  memoryWarning?: string | null
  interactionWarning?: string | null
  /** 对话过长时早期内容被压缩整理过：次数、最近一次时间与前后消息数 */
  compaction?: { count: number; at: number; before: number; after: number; summaryId?: string | null } | null
  memoryStatus?: Record<
    string,
    {
      state: 'running' | 'complete' | 'partial' | 'failed'
      total: number
      completed: number
      attempts: number
      deferred?: number
      failures: Array<{ code: string; message: string; retryable: boolean; source?: Record<string, unknown> }>
    }
  >
  promptDraft?: AgentPromptDraft | null
  updatedAt?: number
  usage?: { input_tokens: number; output_tokens: number; total_tokens: number }
}

export interface AgentWorkspaceSnapshot {
  projects: AgentProject[]
  conversations: AgentConversation[]
  artifacts: AgentArtifact[]
  memories: AgentMemory[]
  skills: AgentSkill[]
  references: AgentReference[]
  jobs?: AgentBatchJob[]
  breakdownJobs?: AgentBreakdownJob[]
  distillationJobs?: AgentDistillationJob[]
  skillSync?: Record<string, unknown>
}

/** 拆书任务：一本参考书的一段章节在后台逐章提取章节卡。进度由宿主按章节卡现算。 */
export interface AgentBreakdownJob {
  id: string
  projectId: string
  /** breakdown:{projectId}，与会话引用的 bookId 对应 */
  bookId: string
  title: string
  from: number
  to: number
  total: number
  done: number
  failed: number
  failedChapters?: Array<{ chapterId: string; message: string }>
  pending: number
  firstTitle?: string | null
  lastTitle?: string | null
  status: 'running' | 'paused' | 'completed' | 'cancelled' | 'failed'
  error?: string | null
  estimate?: { chapters: number; totalTokens: number; minutes: [number, number] }
  remainingMinutes?: [number, number]
  usage?: { input_tokens: number; output_tokens: number; total_tokens: number }
  sessionId?: string | null
  /** 拆完后在发起的会话里自动接着分析；continuedAt 已接着发起，continueSkipped 会话忙没发起的原因 */
  autoContinue?: boolean
  continuedAt?: number | null
  continueSkipped?: string | null
  createdAt?: number
  updatedAt?: number
  finishedAt?: number | null
  revision?: number
}

export interface AgentReferenceImport {
  projectId: string
  bookId: string
  title: string
  chapters: number
  words: number
  /** 同一份文本之前放进来过，复用已存的参考书 */
  existing?: boolean
}

/** 工作备注：模型按作者交代记下的待办与约定，带会话与入口 */
export interface AgentJournalNote {
  id: string
  date: string
  text: string
  sessionId?: string | null
  channel?: string
  bookId?: string | null
  createdAt: number
}

export interface AgentJournalDaySummary {
  date: string
  words: number
  sessions: number
  artifacts: number
  chapters: number
  notes: number
}

/** 某一天的工作：由码字统计、对话、成果、章节改动推出，备注是模型记的 */
export interface AgentJournalDay {
  date: string
  writing: Array<{ bookId: string; title: string; manual: number; ai: number }>
  sessions: Array<{
    sessionId: string
    title: string
    channel: string
    bookId?: string | null
    userMessages: number
    replies: number
    legacy?: string | null
  }>
  artifacts: Array<{ id: string; kind: string; title: string; bookId?: string | null; sessionId?: string | null; createdAt: number }>
  chapters: Array<{ chapterId: string; bookId: string; bookTitle: string; title: string; versions: number; remote: number }>
  notes: AgentJournalNote[]
}

/** 自动任务：定时跑的事，调度器按时间表在宿主后台执行；heartbeat 是内置的助理巡检 */
export interface AgentAutomationSchedule {
  type: 'daily' | 'weekly' | 'interval' | 'once'
  time?: string
  weekday?: number
  minutes?: number
  at?: number
}

export type AgentDeliveryChannel = 'weixin' | 'desktop' | 'none'

export interface AgentAutomationTask {
  id: string
  title: string
  kind: 'agent' | 'heartbeat' | 'command'
  instruction: string
  schedule: AgentAutomationSchedule
  deliver: AgentDeliveryChannel
  bookId?: string | null
  enabled: boolean
  builtin?: boolean
  activeHours?: [number, number]
  createdAt: number
  nextRunAt?: number | null
  lastRunAt?: number | null
  lastStatus?: string
  lastError?: string
  lastSummary?: string
  failures: number
  disabledReason?: string
  estimate?: { perRunTokens: number; runsPerWeek: number; weeklyTokens: number; basis: string } | null
}

export interface AgentAutomationRun {
  taskId: string
  runId: string
  startedAt: number
  finishedAt: number
  status: string
  error?: string
  summary?: string
  merged?: number
  sessionId?: string | null
}

export interface AgentAutomationRule {
  event: string
  /** 说明文字，{参数名} 处由作者填数字 */
  label: string
  enabled: boolean
  deliver: AgentDeliveryChannel
  params?: Record<string, number>
  paramRange?: { name: string; min: number; max: number; default: number }
}

/** 事件提醒的总设置：提醒时段（本地小时）与周期检查状态 */
export interface AgentAutomationSettings {
  activeHours: [number, number]
  checks?: { checkedAt?: number }
}

/** 投递记录：要送到作者面前的一条内容，发出前先存着，发不出去留队重试 */
export interface AgentDeliveryRecord {
  id: string
  channel: string
  text: string
  key?: string
  createdAt: number
  attempts: number
  status: 'pending' | 'sent'
  lastError?: string
  sentAt?: number | null
  source?: Record<string, unknown>
}

export interface AgentMemory {
  id: string
  bookId: string
  content: string
  contentLimit?: number
  category: string
  scope: 'book' | 'author'
  status: 'active' | 'pending' | 'rejected' | 'stale' | 'archived'
  kind?: 'decision' | 'summary' | 'candidate'
  revision?: number
  staleReason?: string
  partial?: boolean
  replacedBy?: string
  sourceRevision?: string
  sourceRef?: {
    type: 'chapter' | 'material' | 'author' | 'artifact'
    bookId: string
    chapterId?: string
    materialKind?: string
    entityId?: number
    sessionId?: string
    messageId?: string
    artifactId?: string
  }
  source: string
  quote: string
  period: string
  /** 作者说这话的时间（作者层偏好按它排序）；手动添加的取添加时间 */
  observedAt?: number
  /** 记下这条记忆的入口：desktop / weixin */
  channel?: string
  /** 从作者修改中归纳的偏好：确认后按这条建立写作规则 */
  suggestedRule?: { category: AgentRuleCategory; text: string }
  /** 规则汇编：固定层超限时列出可删的重复规则与否决约束，确认后才删 */
  digest?: { plan: { rules: string[]; memories: string[] }; savedChars: number }
}

export interface AgentSkill {
  id: string
  name: string
  description: string
  source: 'builtin' | 'custom' | 'third-party'
  enabled: boolean
  python: boolean
  instructions: string
  icon: string
  /** method 选择主写法；style 作为单独选择的文风参考，二者共同进入任务规则集。 */
  kind?: 'method' | 'style' | 'check' | 'tool'
  /** 替换哪个默认方法（如 chapter-writing）；同目标多个启用时要求作者二选一 */
  replaces?: string
  /** 随应用分发或作者目录里的技能文件；有文件的技能可以恢复默认 */
  filePath?: string
  fileSource?: 'builtin' | 'user'
  /** 作者改过说明后内置文件又更新了：这里是新版内置内容 */
  builtinLatest?: string | null
  /** scripts/ 下的 Python 脚本 */
  scripts?: string[]
  /** 作者确认过的脚本版本指纹；第三方脚本改动后要重新确认 */
  approvedHash?: string
  revision?: string
  copiedFrom?: string
}

export interface AgentSkillFile {
  path: string
  content: string
}

/** 人工编辑和 AI 候选共用的保存契约，不接受客户端提供路径、来源或运行授权。 */
export interface AgentSkillDraft {
  id: string
  name: string
  description: string
  instructions: string
  kind?: AgentSkill['kind'] | null
  replaces?: string | null
  revision?: string | null
  files: AgentSkillFile[]
}

export interface AgentSkillStatus {
  scripts: string[]
  requirements: string[]
  depsReady: boolean
  approved: boolean
  scriptHash: string
  directory?: string
  state:
    | 'instructions_only'
    | 'missing_files'
    | 'missing_runtime'
    | 'missing_dependencies'
    | 'pending_confirmation'
    | 'ready'
    | 'failed'
    | 'disabled'
  missingScripts: string[]
  error?: string
  runtime?: { available: boolean; source?: string; version?: string; portableDownloadable?: boolean }
  lastRun?: { at: number; script: string; scriptHash: string; ok: boolean; error?: string }
}

/** 安装或确认前给作者过目的技能内容：说明、每个脚本的全文、依赖与指纹 */
export interface AgentSkillPreview {
  id: string
  name: string
  description: string
  instructions: string
  kind?: AgentSkill['kind']
  replaces?: string
  files: Array<{ path: string; content: string | null; editable: boolean; size: number }>
  requirements: string
  scriptHash: string
  revision: string
  directory: string
  readOnly?: boolean
}

// ---- 作品状态层（记忆页：状态账本、线索、疑点、规则、画像、文风） ----

export interface AgentIndexProgress {
  bookId: string
  indexed: number
  stale: number
  total: number
  pendingEvents: number
  paused: boolean
  pendingEntities?: string[] | null
  foldedAt?: number | null
  lastRunAt?: number | null
  /** 按章节编号记录的提取失败；连续三次后不再自动重试 */
  failed?: Record<string, { count: number; message: string; at: number }> | null
}

/** 宿主推送的索引事件 */
export interface AgentIndexEvent {
  event: 'index_progress' | 'index_failed' | 'index_book' | 'index_finished'
  /** reference：参考书（拆书）通道，结果显示在拆书进度卡上 */
  lane?: 'reference'
  bookId?: string
  chapterId?: string
  message?: string
  summary?: { skipped?: string; completed?: number; givenUp?: number; aborted?: string }
}

export interface AgentCharacterStateValue {
  alive: boolean
  location: string | null
  realm: string | null
  items: string[]
  relations: Record<string, string>
  goals: string[]
}

export interface AgentCharacterState {
  characterId: string
  name: string
  latest: AgentCharacterStateValue
  lastAppearedChapter?: string | null
  appearances: number
  changes: number
  knows: number
}

export interface AgentStateAsOf {
  characterId: string
  name?: string
  asOfChapter: string
  known: boolean
  state?: AgentCharacterStateValue
  knows?: Array<{
    factId: string
    learnedAt: number
    viaNarration: boolean
    quote: string
    chapterId: string
  }>
  lastSeen?: { chapterId: string; flashback: boolean } | null
  changes?: number
}

/** 写作台“状态账本”：写到某章时的人物状态、未回收线索与本章疑点 */
export interface AgentLedger {
  bookId: string
  chapterId: string | null
  /** chapter：按本章章节卡回溯；latest：本章还没有章节卡，用已索引的最新状态 */
  asOf: 'chapter' | 'latest'
  characters: Array<{
    characterId: string
    name: string
    state: AgentCharacterStateValue
    lastSeenChapter: string | null
    lastSeenTitle: string | null
  }>
  threads: Array<{
    threadId: string
    kind: AgentThread['kind']
    title: string
    plantedChapter: string
    plantedTitle: string | null
    lastTouchedTitle: string | null
    idleChapters: number
    overridden?: boolean
  }>
  suspects: AgentSuspect[]
}

export interface AgentFact {
  factId: string
  text: string
  quote: string
  firstChapter: string
}

export interface AgentThread {
  threadId: string
  kind: 'foreshadow' | 'suspense' | 'promise' | 'payoffSetup' | 'romance'
  title: string
  plantedChapter: string
  plantedQuote: string
  advances: Array<{ chapterId: string; action: 'advance' | 'payoff'; text: string; quote: string }>
  status: 'open' | 'paid' | 'dropped'
  paidChapter?: string | null
  note?: string
  /** 作者手动改过状态 */
  overridden?: boolean
}

export interface AgentSuspect {
  issueKey: string
  code: string
  characterId: string
  chapterId: string
  message: string
  /** 两处原文：状态来源与本章；出场类疑点第二处可能为空 */
  quotes: Array<string | null>
}

export interface AgentChapterCardRecord {
  chapterId: string
  status: 'active' | 'stale' | 'orphaned' | 'removed'
  card: { oneLine?: string; narrativeOrder?: number }
  staleReason?: string
  previousCard?: unknown
}

export interface AgentImpactReport {
  chapterId: string
  hasPrevious: boolean
  changes: {
    facts: {
      removed: Array<{ factId: string; text: string }>
      changed: Array<{ factId: string; before: string; after: string }>
      added: Array<{ factId: string; text: string }>
    }
    states: Array<{ characterId: string; field: string; before: string; after: string | null }>
  }
  affected: Array<{ chapterId: string; narrativeOrder: number; reasons: string[]; quotes: string[] }>
  note: string
}

export type AgentRuleCategory = '结构' | '文风' | '禁忌' | '人设' | '平台'

export interface AgentRule {
  id: string
  scope: 'book' | 'author'
  bookId: string
  category: AgentRuleCategory
  text: string
  source: string
  createdAt: number
}

export type AgentPlatformProfile = PlatformProfile & { builtin?: boolean; updatedAt?: number }

export interface AgentRulesetPreview {
  profileName: string
  profileSummary: string
  skillName?: string
  rules: Array<{ id: string; category: string; text: string }>
  expanded: string
  fingerprint: string
}

export type AgentRulesetPreviewResult =
  | AgentRulesetPreview
  | {
      ok: false
      toolError: { code: string; message: string; candidates?: Array<{ id: string; name: string }> }
    }

export interface AgentStyleProfile {
  sampleChars: number
  insufficient: boolean
  metrics: {
    chars: number
    sentenceMean: number
    sentenceStd: number
    paragraphMean: number
    dialogueRatio: number
    exclamationPer1k: number
    ellipsisPer1k: number
    dashPer1k: number
    /** 15 项文风指纹：只给人看的统计，不进提示词 */
    fingerprint?: Record<string, number>
  }
  phrases: Array<{ phrase: string; count: number }>
  autoPoints: string
  points: string
  locked: boolean
  computedAt: number
}

export interface AgentStyleView {
  style: AgentStyleProfile | null
  lock: { points: string } | null
  samples: string[]
  minSampleChars: number
  /** 每章记录过的 AI 插入片段数；这些段落不计入文风样本 */
  aiSpans?: Record<string, number>
}

/** 范围由作者选择，阅读覆盖由宿主按持久节点计算。 */
export interface AgentDistillationJob {
  id: string
  revision: number
  projectId: string
  sessionId: string
  title: string
  focus: string
  status: 'awaiting_scope' | 'running' | 'paused' | 'failed' | 'cancelled' | 'completed'
  phase: 'reading' | 'summarizing' | 'verifying' | 'compiling'
  totalChapters: number
  totalChars: number
  chapterSizes?: number[]
  scope?: { mode: 'full' | 'range'; from: number; to: number }
  selectedChapters: number
  doneChapters: number
  sourceChars?: number
  coveredChars: number
  totalUnits: number
  doneUnits: number
  failedUnits: number
  usage: { input_tokens: number; output_tokens: number; total_tokens: number }
  error?: string | null
  artifactId?: string
}
