import { invokeAgentCommand } from './client'
import { trackStorageWrite } from '@/storage/storage-maintenance'
import type { AgentReferenceImport } from '@/types/agent-creation'
export interface ReferenceImportProgress {
  stage: string
  done: number
  total: number
  cancellable: boolean
}
export type ReferenceImportResult =
  | { material?: false; result: AgentReferenceImport }
  | {
      material: true
      result: { materialId: string; title: string; chars: number }
    }

/** Worker 只持有解析所需的文本；主窗口只接收进度及最多八章的当前保存批次。 */
export function importReferenceFile(
  source: { file: File; intent: 'breakdown' | null } | { projectId: string },
  onProgress: (progress: ReferenceImportProgress) => void
) {
  const worker = new Worker(new URL('./reference-import.worker.ts', import.meta.url), { type: 'module' })
  let token = ''
  let cancelled = false
  let committing = false
  let settled = false
  let inFlight: Promise<unknown> = Promise.resolve()
  let rejectTask: (error: Error) => void = () => undefined
  const cleanup = async () => {
    await inFlight.catch(() => undefined)
    if (token) await invokeAgentCommand('reference_abort', { token })
  }
  const promise = new Promise<ReferenceImportResult>((resolve, reject) => {
    rejectTask = reject
    const fail = (error: Error) => {
      if (settled) return
      settled = true
      worker.terminate()
      void cleanup().then(
        () => reject(error),
        cleanupError => reject(new Error(`${error.message}；暂存清理失败：${cleanupError}`))
      )
    }
    worker.onerror = event => fail(new Error(event.message || '文本处理失败'))
    worker.onmessage = async event => {
      const data = event.data
      if (data.kind === 'progress') onProgress({ ...data, cancellable: data.stage !== '完成导入' })
      else if (data.kind === 'command') {
        if (cancelled) return
        committing = data.action === 'reference_commit' || data.action === 'material_put'
        inFlight = invokeAgentCommand(data.action, data.args)
        try {
          const value = await inFlight
          if (data.action === 'reference_begin') token = (value as { token: string }).token
          if (committing) token = ''
          if (!cancelled) worker.postMessage({ kind: 'reply', request: data.request, value })
        } catch (error) {
          if (!cancelled) worker.postMessage({ kind: 'reply', request: data.request, error: String(error) })
        }
      } else if (data.kind === 'done') {
        settled = true
        worker.terminate()
        resolve({ material: data.material, result: data.result })
      } else if (data.kind === 'error') fail(new Error(data.error))
    }
  })
  const tracked = trackStorageWrite(() => {
    worker.postMessage({ kind: 'start', ...source })
    return promise
  }).catch(error => {
    worker.terminate()
    throw error
  })
  return {
    promise: tracked,
    cancel: async () => {
      if (committing || settled) return
      cancelled = true
      settled = true
      worker.terminate()
      try {
        await cleanup()
        rejectTask(new DOMException('已取消导入', 'AbortError'))
      } catch (error) {
        rejectTask(new Error(`已停止导入，暂存清理失败：${error}`))
      }
    }
  }
}
