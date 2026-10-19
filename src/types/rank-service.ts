import type { RankCollectionRequest, RankGetRequest, RankSourceRequest } from './canon'
import type { NovelRankItem, NovelRankSource } from './novel-rank'

export type { RankCollectionRequest, RankGetRequest, RankSourceRequest }

export interface RankSourceInfo extends NovelRankSource {
  siteCode: string
  siteName: string
  enabled: boolean
  canRefresh: boolean
  latestSnapshotDate: string | null
  earliestSnapshotDate: string | null
  snapshotCount: number
  fetchedAt: number | null
  /** 仅单榜结果附带：最近的快照日期（新到旧，至多 14 个）；目录不列日期以免撑满模型上下文 */
  recentDates?: string[]
}

export interface RankDataResult {
  status: 'available' | 'not_found' | 'refresh_disabled' | 'refresh_failed' | 'cancelled' | 'invalid_request'
  request: RankGetRequest
  source: RankSourceInfo | null
  acquisition: 'refreshed' | 'recent_cache' | 'cached' | 'historical' | 'stale_cache' | 'none'
  freshnessSatisfied: boolean
  completedAt: number
  refresh: { attempted: boolean; retryAfterMs?: number; error?: string }
  snapshot: {
    statDate: string
    fetchedAt: number
    cutoffText: string | null
    pageTitle: string | null
  } | null
  items: NovelRankItem[]
  total: number
  truncated: boolean
}

export interface RankSourcesResult {
  status: 'available'
  sources: RankSourceInfo[]
  total: number
  nextOffset: number | null
  crawlMode: 'off' | 'manual' | 'auto'
  collections: Array<{
    scope: Pick<RankCollectionRequest, 'siteCode' | 'rankType' | 'gender' | 'category'>
    totalSources: number
    genders: string[]
  }>
  filters: {
    platforms: Array<{ code: string; name: string }>
    rankTypes: Array<{ siteCode: string; value: string; label: string }>
  }
}


export interface RankCollectionOptions {
  networkApproved?: boolean
  /** 两次联网抓取之间的停顿（毫秒）；测试可替换为 0 */
  pauseMs?: () => number
}

export interface RankCollectionProgress {
  completed: number
  total: number
}

export type RankBookPreview = Pick<
  NovelRankItem,
  | 'bookId'
  | 'bookTitle'
  | 'bookUrl'
  | 'authorName'
  | 'statusText'
  | 'rankNo'
  | 'metricName'
  | 'metricValue'
  | 'metricText'
>

export interface RankCollectionCategory {
  source: Pick<RankSourceInfo, 'id' | 'title' | 'gender' | 'categoryName' | 'url'>
  status: RankDataResult['status']
  acquisition: RankDataResult['acquisition']
  freshnessSatisfied: boolean
  snapshot: RankDataResult['snapshot']
  refresh: RankDataResult['refresh']
  total: number
  items: RankBookPreview[]
  truncated: boolean
}

export interface RankCollectionGroup {
  metricName: string
  statDate: string
  cutoffText: string | null
  sourceIds: number[]
  uniqueBooks: number
  items: Array<
    RankBookPreview & {
      appearances: Array<{
        sourceId: number
        rankNo: number
        metricValue: number
        metricText?: string | null
      }>
    }
  >
  truncated: boolean
}

export interface RankCollectionResult {
  status:
    | 'available'
    | 'partial'
    | 'not_found'
    | 'unavailable'
    | 'cancelled'
    | 'invalid_request'
    | 'confirmation_required'
  request: RankCollectionRequest
  freshnessSatisfied: boolean
  completedAt: number
  error?: string
  availableRankTypes?: string[]
  /** 需要联网的来源超过阈值时给出，作者同意前不发出任何请求；token、问题与选项由宿主补上 */
  confirmation?: {
    pendingFetches: number
    alreadyFresh: number
    estimatedMinutes: number
    scopeLabel: string
    token?: string
    question?: string
    choices?: string[]
  }
  coverage: RankCollectionProgress & {
    basis: 'configured_sources'
    satisfied: number
    complete: boolean
    failedSourceIds: number[]
    unprocessedSourceIds: number[]
  }
  categories: RankCollectionCategory[]
  aggregation: {
    kind: 'collected_items_by_metric'
    officialOverallRank: false
    itemCoverage: 'captured_snapshots'
    groups: RankCollectionGroup[]
    excludedItems: number
  }
}
