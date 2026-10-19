import { searchBookByMeaning } from '../index/embeddings'

/**
 * 作者文风范例：续写前按语义从本书前文找几段相近的片段当"少样本"，
 * 让模型模仿作者自己的句式与节奏，而不是一段抽象的文风描述。
 * 只在开启向量检索时生效；取范例必须快，超时就不要。
 */

const EXEMPLAR_COUNT = 2
const EXEMPLAR_MAX_CHARS = 320
const TIMEOUT_MS = 4000

const withTimeout = <T>(promise: Promise<T>, ms: number): Promise<T | null> =>
  new Promise(resolve => {
    const timer = setTimeout(() => resolve(null), ms)
    promise.then(value => {
      clearTimeout(timer)
      resolve(value)
    }, () => {
      clearTimeout(timer)
      resolve(null)
    })
  })

export const buildStyleExemplars = async (params: {
  bookId: string | number | undefined
  /** 当前光标前的一段正文，作为语义查询 */
  text: string
  /** 排除当前章节（它就在前文里，不算范例） */
  excludeChapterId?: number | null
}): Promise<string> => {
  if (params.bookId == null || params.bookId === '') return ''
  const query = String(params.text || '').trim().slice(-240)
  if (query.length < 20) return ''
  const result = await withTimeout(
    searchBookByMeaning({ bookId: params.bookId, query, untilChapterId: null, limit: EXEMPLAR_COUNT + 2 }),
    TIMEOUT_MS
  )
  if (!result?.hits.length) return ''
  const picked: string[] = []
  for (const hit of result.hits) {
    if (params.excludeChapterId != null && hit.chapterId === String(params.excludeChapterId)) continue
    const snippet = hit.snippet.replace(/…$/, '').trim()
    if (snippet.length < 24 || picked.some(item => item === snippet)) continue
    picked.push(snippet.slice(0, EXEMPLAR_MAX_CHARS))
    if (picked.length >= EXEMPLAR_COUNT) break
  }
  if (!picked.length) return ''
  return ['【作者文风范例】节选自本书前文，模仿其句式、节奏与用词习惯；不要照抄内容。', ...picked.map((item, index) => `${index + 1}. ${item}`)].join('\n')
}
