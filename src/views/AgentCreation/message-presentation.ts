import type { AgentMessage } from '@/types/agent-creation'

interface MessageGroup {
  id: string
  role: AgentMessage['role']
  runId?: string
  messages: AgentMessage[]
}

export function groupMessages(messages: AgentMessage[]): MessageGroup[] {
  const groups: MessageGroup[] = []
  for (const item of messages) {
    const previous = groups[groups.length - 1]
    // 无文字的“继续任务”也会开启新轮次，不能合并进上一次回复并沿用旧时间。
    const startsReply = item.role === 'assistant' && item.id.endsWith(':reply')
    const sameRun = !item.runId || !previous?.runId || item.runId === previous.runId
    if (item.role === 'assistant' && previous?.role === 'assistant' && sameRun && !startsReply) {
      previous.messages.push(item)
    } else {
      groups.push({ id: item.id, role: item.role, runId: item.runId, messages: [item] })
    }
  }
  return groups
}

const answerTime = new Intl.DateTimeFormat('zh-CN', {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hourCycle: 'h23'
})

export function replyTiming(messages: AgentMessage[]) {
  const reply = messages.find(item => item.id.endsWith(':reply'))
  const start = reply?.createdAt
  const end = reply?.finishedAt
  // 历史消息不使用会话 updatedAt 或下一条消息时间冒充回答时间。
  if (
    typeof start !== 'number' ||
    typeof end !== 'number' ||
    !Number.isFinite(start) ||
    !Number.isFinite(end) ||
    end < start ||
    !Number.isFinite(new Date(end).getTime())
  )
    return null
  const seconds = Math.floor((end - start) / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  const duration =
    seconds === 0
      ? '不到 1 秒'
      : hours
        ? `${hours} 小时${minutes % 60 ? ` ${minutes % 60} 分钟` : ''}`
        : minutes
          ? `${minutes} 分${seconds % 60 ? ` ${seconds % 60} 秒` : '钟'}`
          : `${seconds} 秒`
  return {
    duration: `用时 ${duration}`,
    answeredAt: answerTime.format(end),
    iso: new Date(end).toISOString()
  }
}

/** 作者消息的发送时间，与回答时间同一格式；旧消息没有 createdAt 时不显示，不用其他时间冒充。 */
export function sentTiming(message: AgentMessage) {
  const at = message.createdAt
  if (typeof at !== 'number' || !Number.isFinite(at) || !Number.isFinite(new Date(at).getTime())) return null
  return { sentAt: answerTime.format(at), iso: new Date(at).toISOString() }
}
