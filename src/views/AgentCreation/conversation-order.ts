import type { AgentConversation } from '@/types/agent-creation'

/** 只排序侧栏视图，保留所属项目；会话流式更新不会改变已置顶会话的相对顺序。 */
export function compareConversations(a: AgentConversation, b: AgentConversation) {
  const pinOrder = (b.pinnedAt || 0) - (a.pinnedAt || 0)
  return pinOrder || (b.updatedAt || 0) - (a.updatedAt || 0)
}
