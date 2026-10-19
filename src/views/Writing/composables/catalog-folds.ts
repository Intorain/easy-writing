import { appSettings, flushAppSettings } from '@/storage/app-settings'

/**
 * 目录里各卷的折叠状态，按书记在本地设置里：离开时记下展开的卷，再进来按它恢复；
 * 没记录过的书只展开当前章所在的卷。目录重载（改名、新建、删除后）沿用内存里的状态，不全部展开。
 */
const KEY = 'ew-writing-catalog-folds'
/** 只保留最近用过的这么多本书的记录 */
const KEEP_BOOKS = 50
type FoldStore = Record<string, string[]>

const readStore = (): FoldStore => {
  try {
    const parsed: unknown = JSON.parse(appSettings.getItem(KEY) || '{}')
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? (parsed as FoldStore) : {}
  } catch {
    return {}
  }
}

/** 上次离开这本书时展开的卷；从没记过返回 null */
export const readOpenVolumeIds = (bookId: string): Set<string> | null => {
  const saved = readStore()[String(bookId)]
  return Array.isArray(saved) ? new Set(saved.map(String)) : null
}

export const saveOpenVolumeIds = async (bookId: string, openIds: Iterable<string>) => {
  const store = readStore()
  delete store[String(bookId)]
  const next: FoldStore = Object.fromEntries(Object.entries(store).slice(-(KEEP_BOOKS - 1)))
  next[String(bookId)] = [...new Set([...openIds].map(String))]
  try {
    appSettings.setItem(KEY, JSON.stringify(next))
    await flushAppSettings()
  } catch (error) {
    // 维护期间设置不可写：折叠状态丢一次不影响功能
    console.warn('记录目录折叠状态失败', error)
  }
}

/** 目录重载后一个卷该不该展开：内存里有就沿用，没有就看上次离开时记的，都没有就收着 */
export const resolveVolumeOpen = (volumeId: string, previous: ReadonlyMap<string, boolean>, saved: ReadonlySet<string> | null) => {
  const kept = previous.get(volumeId)
  if (kept !== undefined) return kept
  return saved ? saved.has(volumeId) : false
}
