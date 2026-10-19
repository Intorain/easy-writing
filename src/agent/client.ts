import { isTauriRuntime } from '@/storage/writing-storage'
import { trackStorageWrite } from '@/storage/storage-maintenance'
import type { AgentAutomationTask, AgentDistillationJob, AgentBatchJob, AgentBreakdownJob, AgentConversation, AgentDeliveryRecord, AgentIndexEvent } from '@/types/agent-creation'

/** 已由调用方登记整个复合写入时使用，避免恢复维护在分批导入中途插入。 */
export async function invokeAgentCommand<T>(action: string, args: object = {}): Promise<T> {
  if (!isTauriRuntime()) throw new Error('请在桌面客户端使用 Agent，网页预览不会启动本地后台')
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>('agent_workspace', { action, args })
}

export async function agentCommand<T>(action: string, args: object = {}): Promise<T> {
  const run = () => invokeAgentCommand<T>(action, args)
  return ['load', 'activity', 'stop', 'context', 'canon_query', 'get_session', 'get_artifact', 'writing_profiles', 'ruleset_preview', 'index_status', 'template_recipes', 'breakdown_estimate', 'distillation_read', 'breakdown_jobs', 'breakdown_results', 'breakdown_library', 'reference_export', 'reference_publication', 'automation_list', 'automation_runs', 'automation_rules', 'automation_settings', 'delivery_pending', 'delivery_recent'].includes(action)
    ? run()
    : trackStorageWrite(run)
}

export interface AgentDraftEvent {
  sessionId: string
  runId: string
  artifactId: string
  text: string
}

export type AgentBatchEvent = AgentBatchJob | { jobId: string; event: string; error?: string | null }

/** 监听器属于应用 Store，切换页面不注销；恢复界面时重新读取持久化状态。 */
export async function listenAgent(
  onSession: (session: AgentConversation) => void,
  onDelta: (value: { sessionId: string; runId: string; text: string }) => void,
  extra: {
    onDraft?: (value: AgentDraftEvent) => void
    onBatch?: (value: AgentBatchEvent) => void
    onIndex?: (value: AgentIndexEvent) => void
    onBreakdown?: (value: AgentBreakdownJob) => void
    onDistillation?: (value: AgentDistillationJob) => void
    /** 宿主投递队列里给桌面的一条通知：界面显示后要 delivery_ack 回执 */
    onNotice?: (value: AgentDeliveryRecord) => void
    /** 自动任务跑完一次：任务记录变了 */
    onAutomation?: (value: AgentAutomationTask) => void
  } = {}
): Promise<() => void> {
  const { listen } = await import('@tauri-apps/api/event')
  const listeners = await Promise.all([
    listen<AgentConversation>('agent-session', event => onSession(event.payload)),
    listen<{ sessionId: string; runId: string; text: string }>('agent-delta', event => onDelta(event.payload)),
    listen<AgentDraftEvent>('agent-draft', event => extra.onDraft?.(event.payload)),
    listen<AgentBatchEvent>('agent-batch', event => extra.onBatch?.(event.payload)),
    listen<AgentIndexEvent>('agent-index', event => extra.onIndex?.(event.payload)),
    listen<AgentBreakdownJob>('agent-breakdown', event => extra.onBreakdown?.(event.payload)),
    listen<AgentDistillationJob>('agent-distillation', event => extra.onDistillation?.(event.payload)),
    listen<AgentDeliveryRecord>('agent-notice', event => extra.onNotice?.(event.payload)),
    listen<AgentAutomationTask>('agent-automation', event => extra.onAutomation?.(event.payload))
  ])
  return () => listeners.forEach(stop => stop())
}
