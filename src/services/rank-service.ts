import { RANK_SOURCES, findRankSource, rankPlatformOptions, type RankSeedSource } from '@/config/rank-sources'
import {
  crawlLocalRankSource,
  getLocalRankSettings,
  listSourceSnapshotDates,
  localDate,
  readNewestSnapshot,
  readSnapshot,
  toSourceOption,
  type LocalRankSettings,
  type LocalRankSnapshotDoc
} from '@/storage/local-rank-store'
import { throwIfAborted } from '@/utils/abortable'
import { aggregateRankCollection, previewRankBook } from './rank-collection'
import type {
  RankCollectionOptions,
  RankCollectionProgress,
  RankCollectionRequest,
  RankCollectionResult,
  RankDataResult,
  RankGetRequest,
  RankSourceInfo,
  RankSourceRequest,
  RankSourcesResult
} from '@/types/rank-service'

/** 单榜结果附带的最近日期数；目录只给份数和首末日期 */
const RECENT_DATES = 14
/** 范围采集中需要联网的来源超过此数时，先请作者确认数量与耗时 */
export const COLLECTION_CONFIRM_THRESHOLD = 10

/** 用隐藏窗口渲染的来源（番茄多页懒加载、起点全部）一次只开一个，接近人工浏览的频率 */
const usesRenderWindow = (source: RankSeedSource) =>
  source.siteCode === 'qidian' || (source.siteCode === 'fanqie' && Number(source.meta?.maxPages || 1) > 1)
/** 单源预计耗时（秒），只用于向作者说明，不作为超时依据 */
const estimatedSeconds = (source: RankSeedSource) =>
  source.siteCode === 'qidian' ? 30 : usesRenderWindow(source) ? 20 : 5
const defaultPause = () => 1200 + Math.random() * 800

/** 停顿期间取消立即结束等待，由调用方的循环检查取消状态 */
function pause(ms: number, signal?: AbortSignal) {
  if (ms <= 0 || signal?.aborted) return Promise.resolve()
  return new Promise<void>(resolve => {
    const done = () => {
      clearTimeout(timer)
      signal?.removeEventListener('abort', done)
      resolve()
    }
    const timer = setTimeout(done, ms)
    signal?.addEventListener('abort', done, { once: true })
  })
}

async function sourceInfo(
  sourceId: number,
  options: { recentDates?: boolean; settings?: LocalRankSettings } = {}
): Promise<RankSourceInfo | null> {
  const source = findRankSource(sourceId)
  if (!source) return null
  const dates = await listSourceSnapshotDates(sourceId)
  const latest = dates[0] ? await readSnapshot(sourceId, dates[0]) : null
  const settings = options.settings ?? (await getLocalRankSettings())
  return {
    ...toSourceOption(source),
    siteCode: source.siteCode,
    siteName: rankPlatformOptions().find(site => site.code === source.siteCode)?.name || source.siteCode,
    enabled: source.enabled === 1,
    canRefresh: source.enabled === 1 && settings.mode !== 'off',
    latestSnapshotDate: latest?.statDate ?? null,
    earliestSnapshotDate: dates[dates.length - 1] ?? null,
    snapshotCount: dates.length,
    fetchedAt: latest?.fetchedAt ?? null,
    ...(options.recentDates ? { recentDates: dates.slice(0, RECENT_DATES) } : {})
  }
}

function matchingSources(request: Pick<RankSourceRequest, 'siteCode' | 'gender' | 'rankType' | 'category'>) {
  return RANK_SOURCES.filter(source => {
    const option = toSourceOption(source)
    return (
      source.enabled === 1 &&
      (!request.siteCode || source.siteCode === request.siteCode) &&
      (!request.gender || option.gender === request.gender) &&
      (!request.rankType || source.rankType === request.rankType) &&
      (!request.category ||
        `${option.categoryName || ''} ${option.categoryCode || ''}`.includes(request.category))
    )
  })
}

/** 完整能力目录来自现有源配置；没有抓取过的榜单也能被发现。 */
export async function listRankSources(request: RankSourceRequest = {}): Promise<RankSourcesResult> {
  const settings = await getLocalRankSettings()
  const rankTypes = new Map<string, { siteCode: string; value: string; label: string }>()
  for (const source of RANK_SOURCES.filter(s => s.enabled === 1)) {
    rankTypes.set(`${source.siteCode}:${source.rankType}`, {
      siteCode: source.siteCode,
      value: source.rankType,
      label: source.title || source.rankType
    })
  }
  const matches = matchingSources(request)
  const collections = new Map<string, RankSourcesResult['collections'][number]>()
  for (const source of matches) {
    const key = `${source.siteCode}:${source.rankType}`
    let collection = collections.get(key)
    if (!collection) {
      collection = {
        scope: {
          siteCode: source.siteCode,
          rankType: source.rankType,
          ...(request.gender ? { gender: request.gender } : {}),
          ...(request.category ? { category: request.category } : {})
        },
        totalSources: 0,
        genders: []
      }
      collections.set(key, collection)
    }
    collection.totalSources++
    const gender = toSourceOption(source).gender
    if (gender && !collection.genders.includes(gender)) collection.genders.push(gender)
  }
  const offset = request.offset ?? 0
  const limit = request.limit ?? 30
  const sources = await Promise.all(
    matches.slice(offset, offset + limit).map(source => sourceInfo(source.legacyId, { settings }))
  )
  return {
    status: 'available',
    sources: sources.filter((source): source is RankSourceInfo => source !== null),
    total: matches.length,
    nextOffset: offset + limit < matches.length ? offset + limit : null,
    crawlMode: settings.mode,
    collections: [...collections.values()],
    filters: { platforms: rankPlatformOptions(), rankTypes: [...rankTypes.values()] }
  }
}

function validDate(value: string) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false
  const time = Date.parse(`${value}T00:00:00Z`)
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 10) === value
}

/** 页面与 Agent 共用同一获取流程；时效策略是显式参数，不分析用户话术。 */
export function getRank(request: RankGetRequest, signal?: AbortSignal): Promise<RankDataResult> {
  return loadRank(request, signal, { itemLimit: request.limit ?? 30, recentDates: true })
}

interface LoadOptions {
  itemLimit: number
  /** 单榜：日期列表供模型核对可比较的日期；范围采集不需要 */
  recentDates?: boolean
  /** 范围采集的 latest 表示当天数据：当天已抓过的来源直接复用，不按 30 分钟频控重抓 */
  sameDay?: boolean
}

async function loadRank(
  request: RankGetRequest,
  signal: AbortSignal | undefined,
  options: LoadOptions
): Promise<RankDataResult> {
  const { itemLimit } = options
  const result: RankDataResult = {
    status: 'not_found',
    request,
    source: null,
    acquisition: 'none',
    freshnessSatisfied: false,
    completedAt: Date.now(),
    refresh: { attempted: false },
    snapshot: null,
    items: [],
    total: 0,
    truncated: false
  }
  const attach = (snapshot: LocalRankSnapshotDoc | null) => {
    if (!snapshot) return
    result.snapshot = {
      statDate: snapshot.statDate,
      fetchedAt: snapshot.fetchedAt,
      cutoffText: snapshot.cutoffText || null,
      pageTitle: snapshot.pageTitle || null
    }
    result.total = snapshot.items.length
    result.items = [...snapshot.items].sort((a, b) => a.rankNo - b.rankNo).slice(0, itemLimit)
    result.truncated = result.items.length < result.total
  }
  try {
    throwIfAborted(signal)
    if (
      !Number.isInteger(request.sourceId) ||
      request.sourceId < 1 ||
      !['latest', 'cached', 'historical'].includes(request.freshness) ||
      (request.limit !== undefined &&
        (!Number.isInteger(request.limit) || request.limit < 1 || request.limit > 100)) ||
      (request.freshness === 'historical' ? !validDate(request.statDate || '') : Boolean(request.statDate))
    ) {
      result.status = 'invalid_request'
      result.refresh.error =
        '历史查询需要有效日期，最新或本地查询不接受历史日期；榜单编号与条目数量必须有效。'
      return result
    }
    result.source = await sourceInfo(request.sourceId, options)
    if (!result.source) return result
    if (request.freshness === 'historical' || request.freshness === 'cached') {
      attach(
        request.freshness === 'historical'
          ? await readSnapshot(request.sourceId, request.statDate!)
          : await readNewestSnapshot(request.sourceId)
      )
      result.status = result.snapshot ? 'available' : 'not_found'
      result.acquisition = result.snapshot ? request.freshness : 'none'
      result.freshnessSatisfied = Boolean(result.snapshot)
      return result
    }
    if (!result.source.canRefresh) {
      attach(await readNewestSnapshot(request.sourceId))
      result.status = 'refresh_disabled'
      result.acquisition = result.snapshot ? 'stale_cache' : 'none'
      return result
    }
    const outcome = await crawlLocalRankSource(request.sourceId, {
      manual: !options.sameDay,
      signal,
      onFetchStart: () => {
        result.refresh.attempted = true
      }
    })
    throwIfAborted(signal)
    result.refresh.attempted = outcome.crawled
    result.refresh.retryAfterMs = outcome.retryAfterMs
    if (outcome.reason === 'disabled') {
      result.status = 'refresh_disabled'
      attach(await readNewestSnapshot(request.sourceId))
      result.acquisition = result.snapshot ? 'stale_cache' : 'none'
      return result
    }
    attach(outcome.snapshotDate ? await readSnapshot(request.sourceId, outcome.snapshotDate) : null)
    result.status = result.snapshot ? 'available' : 'not_found'
    result.acquisition = result.snapshot ? (outcome.crawled ? 'refreshed' : 'recent_cache') : 'none'
    result.freshnessSatisfied = Boolean(result.snapshot)
    if (outcome.crawled) result.source = await sourceInfo(request.sourceId, options)
    return result
  } catch (error) {
    const cancelled = signal?.aborted || (error instanceof DOMException && error.name === 'AbortError')
    result.status = cancelled ? 'cancelled' : 'refresh_failed'
    result.refresh = {
      attempted: result.refresh.attempted,
      error: error instanceof Error ? error.message : String(error)
    }
    if (!cancelled) {
      attach(await readNewestSnapshot(request.sourceId))
      result.acquisition = result.snapshot ? 'stale_cache' : 'none'
    }
    return result
  } finally {
    result.completedAt = Date.now()
  }
}

/** 执行层负责完整遍历与覆盖核算；模型负责选择范围和解释数据。 */
export async function getRankCollection(
  input: RankCollectionRequest,
  signal?: AbortSignal,
  onProgress?: (progress: RankCollectionProgress) => Promise<void>,
  options: RankCollectionOptions = {}
): Promise<RankCollectionResult> {
  const request = { ...input, freshness: input.freshness ?? 'latest' }
  const result: RankCollectionResult = {
    status: 'not_found',
    request,
    freshnessSatisfied: false,
    completedAt: Date.now(),
    coverage: {
      basis: 'configured_sources',
      total: 0,
      completed: 0,
      satisfied: 0,
      complete: false,
      failedSourceIds: [],
      unprocessedSourceIds: []
    },
    categories: [],
    aggregation: {
      kind: 'collected_items_by_metric',
      officialOverallRank: false,
      itemCoverage: 'captured_snapshots',
      groups: [],
      excludedItems: 0
    }
  }
  if (
    !['fanqie', 'qidian', 'qimao'].includes(request.siteCode) ||
    !request.rankType?.trim() ||
    (request.gender !== undefined && !['male', 'female'].includes(request.gender)) ||
    !['latest', 'cached', 'historical'].includes(request.freshness) ||
    (request.limit !== undefined &&
      (!Number.isInteger(request.limit) || request.limit < 1 || request.limit > 100)) ||
    (request.perCategoryLimit !== undefined &&
      (!Number.isInteger(request.perCategoryLimit) ||
        request.perCategoryLimit < 1 ||
        request.perCategoryLimit > 10)) ||
    (request.freshness === 'historical' ? !validDate(request.statDate || '') : Boolean(request.statDate))
  ) {
    result.status = 'invalid_request'
    result.error = '平台、榜型、时效或展示数量无效；历史查询需要有效日期，其他查询不接受历史日期。'
    return result
  }
  const availableRankTypes = [
    ...new Set(matchingSources({ siteCode: request.siteCode }).map(s => s.rankType))
  ]
  if (!availableRankTypes.includes(request.rankType)) {
    result.status = 'invalid_request'
    result.availableRankTypes = availableRankTypes
    result.error = '该平台不支持这个榜型，请使用目录中的 availableRankTypes。'
    return result
  }
  const sources = matchingSources(request)
  result.coverage.total = sources.length
  result.coverage.unprocessedSourceIds = sources.map(source => source.legacyId)
  if (!sources.length) {
    result.availableRankTypes = [
      ...new Set(matchingSources({ siteCode: request.siteCode }).map(s => s.rankType))
    ]
    result.error = '当前筛选范围没有匹配来源；可用榜型见 availableRankTypes，分类和频道可通过目录核对。'
    return result
  }
  // 先核算需要联网的来源：latest 且允许抓取时，当天还没有快照的来源才会发出请求。
  const settings = await getLocalRankSettings()
  const today = localDate()
  const pending =
    request.freshness === 'latest' && settings.mode !== 'off'
      ? (
          await Promise.all(
            sources.map(async source =>
              (await listSourceSnapshotDates(source.legacyId))[0] === today ? null : source
            )
          )
        ).filter((source): source is RankSeedSource => source !== null)
      : []
  const concurrency = pending.some(usesRenderWindow) ? 1 : 2
  if (pending.length > COLLECTION_CONFIRM_THRESHOLD && !options.networkApproved) {
    const seconds = pending.reduce((sum, source) => sum + estimatedSeconds(source) + 1.6, 0) / concurrency
    const minutes = Math.max(1, Math.ceil(seconds / 60))
    result.status = 'confirmation_required'
    const site = rankPlatformOptions().find(platform => platform.code === request.siteCode)?.name || request.siteCode
    const channel = request.gender ? (request.gender === 'male' ? '男频' : '女频') : ''
    result.confirmation = {
      pendingFetches: pending.length,
      alreadyFresh: sources.length - pending.length,
      estimatedMinutes: minutes,
      scopeLabel: `${site}${channel}${sources[0]!.title}${request.category ? `·${request.category}` : ''}`
    }
    result.error = `本次范围有 ${pending.length} 个榜单需要联网抓取，预计约 ${minutes} 分钟，作者确认后才会开始。`
    return result
  }
  const wait = options.pauseMs ?? defaultPause
  const data: RankDataResult[] = []
  let cursor = 0
  // 复用单源频控与快照，不向模型展开逐分类循环。隐藏窗口来源一次一个，纯接口来源两条并行。
  const consume = async () => {
    while (!signal?.aborted && cursor < sources.length) {
      const source = sources[cursor++]!
      let item: RankDataResult
      try {
        item = await loadRank(
          { sourceId: source.legacyId, freshness: request.freshness, statDate: request.statDate },
          signal,
          { itemLimit: Number.POSITIVE_INFINITY, sameDay: true }
        )
      } catch (error) {
        // 单源存储/读取失败不能中断其他分类的覆盖核算。
        item = {
          status: signal?.aborted ? 'cancelled' : 'refresh_failed',
          request: { sourceId: source.legacyId, freshness: request.freshness },
          source: null,
          acquisition: 'none',
          freshnessSatisfied: false,
          snapshot: null,
          items: [],
          total: 0,
          truncated: false,
          completedAt: Date.now(),
          refresh: { attempted: false, error: String(error) }
        }
      }
      data.push(item)
      result.coverage.completed++
      result.coverage.unprocessedSourceIds = result.coverage.unprocessedSourceIds.filter(
        id => id !== source.legacyId
      )
      if (item.freshnessSatisfied) result.coverage.satisfied++
      else result.coverage.failedSourceIds.push(source.legacyId)
      result.categories.push({
        source: {
          id: source.legacyId,
          title: source.title,
          url: source.url,
          categoryName: toSourceOption(source).categoryName,
          gender: toSourceOption(source).gender
        },
        status: item.status,
        acquisition: item.acquisition,
        freshnessSatisfied: item.freshnessSatisfied,
        snapshot: item.snapshot,
        refresh: item.refresh,
        total: item.total,
        items: item.items.slice(0, request.perCategoryLimit ?? 3).map(previewRankBook),
        truncated: item.total > (request.perCategoryLimit ?? 3)
      })
      await onProgress?.({ completed: result.coverage.completed, total: sources.length })
      // 真正联网后停顿再取下一个来源；只读本地的来源不停顿
      if (item.refresh.attempted && cursor < sources.length) await pause(wait(), signal)
    }
  }
  await onProgress?.({ completed: 0, total: sources.length })
  await Promise.all(Array.from({ length: concurrency }, () => consume()))
  result.categories.sort((a, b) => a.source.id - b.source.id)
  result.coverage.failedSourceIds.sort((a, b) => a - b)
  result.coverage.complete = result.coverage.satisfied === sources.length
  result.freshnessSatisfied = result.coverage.complete
  result.status = signal?.aborted
    ? 'cancelled'
    : result.coverage.complete
      ? 'available'
      : result.coverage.satisfied > 0
        ? 'partial'
        : 'unavailable'
  // 旧缓存可以在分类结果里供追查，但不能参与本次时效要求下的汇总。
  result.aggregation = aggregateRankCollection(data, request.limit ?? 20)
  result.completedAt = Date.now()
  return result
}
