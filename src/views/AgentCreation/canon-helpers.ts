/** 记忆页状态层面板共用的小工具：章节标题、枚举文案、时间格式。 */
import type { AgentCharacterStateValue, AgentReference, AgentThread } from '@/types/agent-creation'

/** 宿主按目录顺序给出章节引用；返回 chapterId → 标题，顺序即目录顺序 */
export function chapterTitleMap(references: AgentReference[], bookId: string) {
  const map = new Map<string, string>()
  for (const item of references)
    if (item.kind === 'chapter' && item.bookId === bookId && item.chapterId)
      map.set(item.chapterId, item.title)
  return map
}

export const THREAD_KIND_LABELS: Record<AgentThread['kind'], string> = {
  foreshadow: '伏笔',
  suspense: '悬念',
  promise: '承诺',
  payoffSetup: '爽点铺垫',
  romance: '感情线'
}

export const THREAD_STATUS_LABELS: Record<AgentThread['status'], string> = {
  open: '未回收',
  paid: '已回收',
  dropped: '已放弃'
}

export function formatStamp(value?: number | null) {
  if (!value) return ''
  return new Date(value).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  })
}

/** 一行概括人物当前状态，没有提取到的字段不显示 */
export function stateLine(state?: AgentCharacterStateValue | null) {
  if (!state) return '暂无状态记录'
  const parts = [
    state.realm ? `境界：${state.realm}` : '',
    state.location ? `在：${state.location}` : '',
    state.items?.length
      ? `持有：${state.items.slice(0, 3).join('、')}${state.items.length > 3 ? '等' : ''}`
      : ''
  ].filter(Boolean)
  return parts.join(' · ') || '暂无状态变化'
}
