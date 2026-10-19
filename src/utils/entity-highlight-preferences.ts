/** 取消高亮记录按作品分组；名称也是别名、同名角色/设定共用的匹配键。 */
export const ENTITY_HIGHLIGHT_DISMISS_STORAGE_KEY = 'ew-writing-entity-highlight-dismissed'
export type DismissedEntityHighlights = Record<string, string[]>

export const parseDismissedEntityHighlights = (raw?: string | null): DismissedEntityHighlights => {
  try {
    const value: unknown = JSON.parse(raw || '{}')
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {}
    return Object.fromEntries(
      Object.entries(value).flatMap(([bookId, names]) => {
        if (!Array.isArray(names)) return []
        const normalized = [
          ...new Set(
            names
              .filter((name): name is string => typeof name === 'string')
              .map(name => name.trim())
              .filter(Boolean)
          )
        ]
        return normalized.length ? [[bookId, normalized]] : []
      })
    )
  } catch {
    return {}
  }
}

/** 仅给备份中的作品换号，再按作品合并名称；不覆盖本机其他作品的取消记录。 */
export const mergeDismissedEntityHighlights = (
  current: string | null | undefined,
  backup: string,
  remapBookId: (id: string) => string
): string => {
  const merged = new Map(Object.entries(parseDismissedEntityHighlights(current)))
  for (const [bookId, names] of Object.entries(parseDismissedEntityHighlights(backup))) {
    const target = remapBookId(bookId)
    merged.set(target, [...new Set([...(merged.get(target) || []), ...names])])
  }
  return JSON.stringify(Object.fromEntries(merged))
}
