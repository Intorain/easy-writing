import type { NovelRankItem } from '@/types/novel-rank'
import type {
  RankBookPreview,
  RankCollectionGroup,
  RankCollectionResult,
  RankDataResult
} from '@/types/rank-service'

export function previewRankBook(item: NovelRankItem): RankBookPreview {
  const { bookId, bookTitle, bookUrl, authorName, statusText, rankNo, metricName, metricValue, metricText } =
    item
  return { bookId, bookTitle, bookUrl, authorName, statusText, rankNo, metricName, metricValue, metricText }
}

const AGENT_INTRO_CHARS = 80

/**
 * 交给模型的单榜条目：保留判断题材与热度需要的字段，简介截短，去掉封面、章节链接等页面展示字段。
 * 完整条目仍在快照库，榜单页照常读取。
 */
export function agentRankItem(item: NovelRankItem) {
  const intro = item.intro?.trim()
  return {
    ...previewRankBook(item),
    ...(item.rankChange ? { rankChange: item.rankChange } : {}),
    ...(item.categoryName ? { categoryName: item.categoryName } : {}),
    ...(intro
      ? { intro: intro.length > AGENT_INTRO_CHARS ? `${intro.slice(0, AGENT_INTRO_CHARS)}…` : intro }
      : {})
  }
}

export function agentRankResult(result: RankDataResult) {
  return { ...result, items: result.items.map(agentRankItem) }
}

/** 指标与时间口径一致才合并，保留分类内排名；同书出现多次不累加在读数。 */
export function aggregateRankCollection(
  data: RankDataResult[],
  limit: number
): RankCollectionResult['aggregation'] {
  const groups = new Map<
    string,
    { group: RankCollectionGroup; books: Map<string, RankCollectionGroup['items'][number]> }
  >()
  let excludedItems = 0
  for (const result of data) {
    const { source, snapshot } = result
    if (!result.freshnessSatisfied || !source || !snapshot) {
      excludedItems += result.items.length
      continue
    }
    for (const item of result.items) {
      if (
        !item.metricName ||
        !item.metricText ||
        typeof item.metricValue !== 'number' ||
        !Number.isFinite(item.metricValue) ||
        item.metricValue < 0
      ) {
        excludedItems++
        continue
      }
      // 缺少平台截止信息时只在单源内比较，不以本地抓取日冒充统计口径。
      const key = JSON.stringify([
        source.siteCode,
        source.rankType,
        snapshot.statDate,
        snapshot.cutoffText || source.id,
        item.metricName
      ])
      let entry = groups.get(key)
      if (!entry) {
        entry = {
          group: {
            metricName: item.metricName,
            statDate: snapshot.statDate,
            cutoffText: snapshot.cutoffText,
            sourceIds: [],
            uniqueBooks: 0,
            items: [],
            truncated: false
          },
          books: new Map()
        }
        groups.set(key, entry)
      }
      if (!entry.group.sourceIds.includes(source.id)) entry.group.sourceIds.push(source.id)
      // 无可靠标识时不按书名合并，避免同名书被误去重。
      const bookKey = item.bookId
        ? `id:${item.bookId}`
        : item.bookUrl
          ? `url:${item.bookUrl}`
          : `${source.id}:${item.rankNo}`
      const appearance = {
        sourceId: source.id,
        rankNo: item.rankNo,
        metricValue: item.metricValue,
        metricText: item.metricText
      }
      const existing = entry.books.get(bookKey)
      if (existing) {
        existing.appearances.push(appearance)
        // 同口径重复观测按较大可见值排序，原始观测值仍逐一保留。
        if (item.metricValue > (existing.metricValue ?? 0)) Object.assign(existing, previewRankBook(item))
      } else entry.books.set(bookKey, { ...previewRankBook(item), appearances: [appearance] })
    }
  }
  const entries = [...groups.values()].sort(
    (a, b) => Math.min(...a.group.sourceIds) - Math.min(...b.group.sourceIds)
  )
  const candidates = entries.map(({ books }) =>
    [...books.values()].sort(
      (a, b) => (b.metricValue ?? 0) - (a.metricValue ?? 0) || a.bookTitle.localeCompare(b.bookTitle)
    )
  )
  // limit 是总展示预算，轮流取各口径组的靠前条目；口径分组多时也不把模型上下文撑满。
  let remaining = limit
  for (let rank = 0; remaining > 0 && candidates.some(items => items.length > rank); rank++) {
    for (let index = 0; index < entries.length && remaining > 0; index++) {
      const item = candidates[index]?.[rank]
      if (item) {
        entries[index]!.group.items.push(item)
        remaining--
      }
    }
  }
  return {
    kind: 'collected_items_by_metric',
    officialOverallRank: false,
    itemCoverage: 'captured_snapshots',
    excludedItems,
    groups: entries.map(({ group, books }) => ({
      ...group,
      sourceIds: group.sourceIds.sort((a, b) => a - b),
      uniqueBooks: books.size,
      truncated: group.items.length < books.size
    }))
  }
}
