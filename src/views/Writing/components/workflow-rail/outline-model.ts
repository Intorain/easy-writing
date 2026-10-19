import type { JsonRecord } from '@/types/json'

/**
 * 大纲面板的数据模型与归一化工具。
 *
 * 数据来源：volumeTree（工作流视图）返回的卷/章节点，节点即章节实体展开，
 * 自带 planMeta（含 expandedOutline 细纲与 outlineSource 标记）。
 * 细纲的拍数/长度边界与服务端 outline_expander 完全一致，
 * 否则保存的细纲会被服务端 normalize 判为无效并在生成时被覆盖。
 */

/** 与服务端 outline_expander 对齐的细纲边界 */
export const OUTLINE_MIN_BEATS = 3
export const OUTLINE_MAX_BEATS = 10
export const OUTLINE_BEAT_MAX_LEN = 80

export interface OutlineChapterNode {
  id: number
  volumeId: string
  sortNo: number
  title: string
  summary: string
  workflowStatus?: string
  planMeta: JsonRecord | null
}

/** 卷计划里尚未生成为真实章节的未来章（只读展示） */
export interface OutlinePlannedChapter {
  planIndex: number
  title: string
  summary: string
}

export interface OutlineVolumeNode {
  id: number
  sortNo: number
  title: string
  summary: string
  chapters: OutlineChapterNode[]
  plannedChapters: OutlinePlannedChapter[]
}

export interface ExpandedOutlineDraft {
  beats: string[]
  /** 与 beats 等长的场景标签；无标签数据时为 null，保存时不落该字段 */
  beatScenes: string[] | null
  endHook: string
}

const readNodeText = (value: unknown) => (typeof value === 'string' ? value : '')

const isRecord = (value: unknown): value is JsonRecord =>
  Boolean(value) && typeof value === 'object' && !Array.isArray(value)

/**
 * 一拍的文字：旧细纲是字符串；Agent 的拍子（BeatsPlan）是对象，取“［用途］场景｜冲突”。
 * 章末钩子同理，对象取 text。
 */
export const beatLine = (beat: unknown): string => {
  if (typeof beat === 'string') return beat.trim()
  if (!isRecord(beat)) return ''
  const scene = readNodeText(beat.scene).trim()
  if (!scene) return ''
  const purpose = readNodeText(beat.purpose).trim()
  const conflict = readNodeText(beat.conflict).trim()
  return `${purpose ? `［${purpose}］` : ''}${scene}${conflict ? `｜冲突：${conflict}` : ''}`
}

export const hookLine = (hook: unknown): string =>
  typeof hook === 'string' ? hook.trim() : isRecord(hook) ? readNodeText(hook.text).trim() : ''

/** 读取节点上的细纲；无效（拍数不足等）视为未展开 */
export const normalizeExpandedOutline = (
  raw: unknown
): ExpandedOutlineDraft | null => {
  if (!isRecord(raw) || !Array.isArray(raw.beats)) return null
  const rawScenes = Array.isArray(raw.beatScenes) ? raw.beatScenes : []
  const beats: string[] = []
  const scenes: string[] = []
  raw.beats.forEach((beat, index) => {
    const text = beatLine(beat).slice(0, OUTLINE_BEAT_MAX_LEN)
    if (!text) return
    beats.push(text)
    scenes.push(readNodeText(rawScenes[index]))
  })
  if (!beats.length) return null
  return {
    beats: beats.slice(0, OUTLINE_MAX_BEATS),
    beatScenes: scenes.some(Boolean) ? scenes.slice(0, OUTLINE_MAX_BEATS) : null,
    endHook: hookLine(raw.endHook).slice(0, OUTLINE_BEAT_MAX_LEN),
  }
}

/** 章纲是否已被人工接管（AI 不再覆盖） */
export const isOutlineUserLocked = (planMeta: JsonRecord | null | undefined) =>
  String(planMeta?.outlineSource || '') === 'user'

/** 客户端复刻服务端 hasWorkflowPlanIdentity：识别由工作流计划生成的章 */
const hasWorkflowPlanIdentity = (planMeta: JsonRecord | null | undefined) => {
  const meta = planMeta || {}
  const rawPlanIndex = meta.workflowPlanIndex
  const source = meta.source
  const hasSource =
    source !== null &&
    source !== undefined &&
    (typeof source !== 'object' || Object.keys(source as Record<string, unknown>).length > 0)
  return (
    (rawPlanIndex !== null && rawPlanIndex !== undefined && rawPlanIndex !== '') ||
    Boolean(String(meta.id || '').trim()) ||
    hasSource ||
    meta.workflowMaterialized === true ||
    meta.workflowPartial === true ||
    ['volume_ai', 'phase_a'].includes(String(meta.outlineSource || ''))
  )
}

const readPlanEntryText = (plan: JsonRecord, keys: string[]) => {
  for (const key of keys) {
    const direct = readNodeText(plan?.[key]).trim()
    if (direct) return direct
    const nested = readNodeText(plan?.source?.[key]).trim()
    if (nested) return nested
  }
  return ''
}

/**
 * 从卷计划中挑出尚未生成的未来章：已生成章按 workflowPlanIndex / 计划 id /
 * 卷内顺位认领计划下标，剩余下标即"待生成"。认领逻辑与服务端
 * resolveChapterPlanIndex 的主路径一致，匹配不到时宁可少展示、不重复展示。
 */
export const extractPlannedChapters = (
  volumePlanMeta: JsonRecord | null | undefined,
  chapters: OutlineChapterNode[]
): OutlinePlannedChapter[] => {
  const plans = Array.isArray(volumePlanMeta?.chapters) ? volumePlanMeta!.chapters : []
  if (!plans.length) return []
  const claimed = new Set<number>()
  const workflowChapters = chapters.filter(chapter => hasWorkflowPlanIdentity(chapter.planMeta))
  workflowChapters.forEach((chapter, position) => {
    const rawIndex = chapter.planMeta?.workflowPlanIndex
    const explicit = Number(rawIndex)
    if (
      rawIndex !== null &&
      rawIndex !== undefined &&
      rawIndex !== '' &&
      Number.isInteger(explicit) &&
      explicit >= 0
    ) {
      claimed.add(explicit)
      return
    }
    const planId = String(chapter.planMeta?.id || '').trim()
    if (planId) {
      const matched = plans.findIndex(
        (plan: JsonRecord) => String(plan?.id || '').trim() === planId
      )
      if (matched >= 0) {
        claimed.add(matched)
        return
      }
    }
    // 旧数据没有稳定下标：按工作流章的卷内顺位保守认领，避免计划重复投影
    claimed.add(position)
  })
  return plans
    .map((plan: JsonRecord, planIndex: number) => ({
      planIndex,
      title: readPlanEntryText(plan || {}, ['title']),
      summary: readPlanEntryText(plan || {}, ['summary', 'goal', 'outline']),
    }))
    .filter(
      (plan: OutlinePlannedChapter) =>
        !claimed.has(plan.planIndex) && (plan.title || plan.summary)
    )
}

export const normalizeOutlineVolumes = (raw: unknown): OutlineVolumeNode[] => {
  if (!Array.isArray(raw)) return []
  return raw
    .filter(volume => isRecord(volume))
    .map(volume => {
      const chapters: OutlineChapterNode[] = (Array.isArray(volume.children) ? volume.children : [])
        .filter((chapter: JsonRecord) => isRecord(chapter) && Number(chapter.id))
        .map((chapter: JsonRecord) => ({
          id: Number(chapter.id),
          volumeId: String(chapter.volumeId || ''),
          sortNo: Number(chapter.sortNo || 0),
          title: readNodeText(chapter.title),
          summary: readNodeText(chapter.summary),
          workflowStatus: readNodeText(chapter.workflowStatus) || undefined,
          planMeta: isRecord(chapter.planMeta) ? chapter.planMeta : null,
        }))
      return {
        id: Number(volume.id),
        sortNo: Number(volume.sortNo || 0),
        title: readNodeText(volume.title),
        summary: readNodeText(volume.summary),
        chapters,
        plannedChapters: extractPlannedChapters(
          isRecord(volume.planMeta) ? volume.planMeta : null,
          chapters
        ),
      }
    })
}

const CHAPTER_ORDINAL_PATTERN = /^第[0-9一二三四五六七八九十百千万零〇两]+[章节回]/

/** 工作流旧建书标题可能只存事件名，展示层补齐章节序号（与目录树一致） */
export const formatChapterDisplayTitle = (chapter: Pick<OutlineChapterNode, 'title' | 'sortNo'>) => {
  const title = String(chapter.title || '').trim() || '未命名章节'
  if (CHAPTER_ORDINAL_PATTERN.test(title)) return title
  const sortNo = Number(chapter.sortNo || 0)
  return sortNo > 0 ? `第${sortNo}章 ${title}` : title
}
