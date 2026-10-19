import type { AiTextSearchResult } from '@/storage/ai-index'
import { agentCommand } from '@/agent/client'
import { isTauriRuntime } from '@/storage/writing-storage'

/**
 * 语义（向量）检索：向量由宿主的索引器用模型管理里的向量模型（分组 embedding）生成并存进 SQLite，
 * 检索也在宿主里算；这里只是入口。没有配置向量模型时宿主返回 unavailable，调用方退回关键词结果。
 */

export const getEmbeddingModel = async (): Promise<string> => {
  const { useAiModelStore } = await import('@/stores/ai-model')
  return useAiModelStore().ensureGroupModel('embedding')
}

/** 让索引器尽快补这本书缺的向量：索引器每轮结束会顺手补，这里只是催一下 */
export const ensureBookEmbeddings = (_bookId: string | number) => {
  if (!isTauriRuntime()) return
  void agentCommand('index_now').catch(() => undefined)
}

export const searchBookByMeaning = async (params: {
  bookId: string | number
  query: string
  untilChapterId?: string | number | null
  limit?: number
  signal?: AbortSignal
}): Promise<AiTextSearchResult | null> => {
  if (!isTauriRuntime()) return null
  const query = String(params.query || '').trim()
  if (!query) return null
  const result = await agentCommand<AiTextSearchResult & { unavailable?: boolean }>('canon_query', {
    bookId: String(params.bookId),
    view: 'search',
    mode: 'semantic',
    query,
    untilChapterId: params.untilChapterId == null ? '' : String(params.untilChapterId),
    limit: params.limit,
  })
  if (result.unavailable) return null
  return { hits: result.hits, scannedChapters: result.scannedChapters, indexedChapters: result.indexedChapters }
}
