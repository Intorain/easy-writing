import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getRank, getRankCollection, listRankSources } from '@/services/rank-service'
import { agentRankResult } from '@/services/rank-collection'
import type { RankCollectionRequest, RankGetRequest, RankSourceRequest } from '@/types/rank-service'
import { textToBase64 } from './media'

/**
 * 网页桥执行器：宿主把只能在网页层完成的调用发过来，这里执行后把结果交还同一请求编号。
 * 榜单抓取复用榜单页的服务；灵感便签与码字统计的数据在前端设置缓存里；发文件、发图走微信媒体通道，画图用 echarts；
 * 按封面方案出图调本地图片模型（见 cover-image.ts）。
 * 属于应用生命周期，不随榜单页或 Agent 页卸载。
 */

export type WebviewTool =
  | 'list_rank_sources'
  | 'get_rank'
  | 'get_rank_collection'
  | 'send_chapter_file'
  | 'send_rank_chart'
  | 'list_inspirations'
  | 'add_inspiration'
  | 'writing_stats'
  | 'generate_cover'

export interface WebviewExecutorRequest {
  id: string
  clientId: string
  tool: WebviewTool
  request: Record<string, unknown>
  /** 宿主核对作者已同意大范围联网抓取；不来自模型参数 */
  networkApproved?: boolean
  /** 发起这次调用的对话入口（desktop / weixin） */
  channel?: string
}

export const WEBVIEW_REQUEST_EVENT = 'agent-webview-request'
export const WEBVIEW_CANCEL_EVENT = 'agent-webview-cancel'

type Progress = (progress: { completed: number; total: number }) => Promise<void>
type Handler = (request: WebviewExecutorRequest, signal: AbortSignal, progress: Progress) => Promise<Record<string, unknown>>

const RANK_TOOLS = new Set<WebviewTool>(['list_rank_sources', 'get_rank', 'get_rank_collection'])
const text = (value: unknown) => String(value ?? '').trim()
const toolError = (code: string, message: string) => ({ ok: false, toolError: { code, retryable: false, message } })
const GENDER_LABEL: Record<string, string> = { male: '男频', female: '女频' }

/** 发到微信的文件与图片只在微信对话进行中才有去处 */
const sendToWeixin = async (request: WebviewExecutorRequest, media: { kind: 'image' | 'file'; fileName: string; dataBase64: string }) => {
  if (request.channel !== 'weixin') throw new Error('当前对话入口不能发文件和图片')
  const { sendWeixinMedia } = await import('@/ai/channels/weixin/channel')
  await sendWeixinMedia(media)
}

const HANDLERS: Record<WebviewTool, Handler> = {
  list_rank_sources: async request => (await listRankSources(request.request as unknown as RankSourceRequest)) as unknown as Record<string, unknown>,
  get_rank: async (request, signal) => agentRankResult(await getRank(request.request as unknown as RankGetRequest, signal)) as unknown as Record<string, unknown>,
  get_rank_collection: async (request, signal, progress) =>
    (await getRankCollection(request.request as unknown as RankCollectionRequest, signal, progress, {
      networkApproved: request.networkApproved === true,
    })) as unknown as Record<string, unknown>,
  send_chapter_file: async request => {
    const fileName = text(request.request.fileName)
    const body = String(request.request.text ?? '')
    const summary = text(request.request.summary) || fileName
    const words = Number(request.request.words || 0)
    await sendToWeixin(request, { kind: 'file', fileName, dataBase64: textToBase64(body) })
    return { sent: true, fileName, summary, words, message: `已把${summary}导出为 ${fileName}（${words} 字）发给作者，文件在这条消息上方。` }
  },
  send_rank_chart: async request => {
    const siteCode = text(request.request.site_code)
    const gender = text(request.request.gender)
    const days = Math.max(2, Math.min(Number(request.request.days || 7), 60))
    const [{ getLocalRankTagTrends }, { rankPlatformOptions }] = await Promise.all([
      import('@/storage/local-rank-analysis'),
      import('@/config/rank-sources'),
    ])
    const { data } = await getLocalRankTagTrends({ siteCode, gender: gender || undefined, rankType: text(request.request.rank_type) || undefined, days, topN: 8 })
    const platform = rankPlatformOptions().find(item => item.code === siteCode)?.name || siteCode
    if (!data.list.length) {
      return { sent: false, status: 'no_data', message: `本机还没有${platform}的榜单快照，画不出趋势图；需要先用 get_rank 抓取。` }
    }
    const title = `${platform}${gender ? GENDER_LABEL[gender] || gender : ''}标签趋势 ${data.startDate} ~ ${data.endDate}`
    // echarts 只在真要画图时才加载
    const { buildTagTrendChartOption, renderChartPng } = await import('./rank-chart')
    const png = await renderChartPng(buildTagTrendChartOption(data, title))
    await sendToWeixin(request, { kind: 'image', fileName: `${siteCode}-trend-${data.endDate}.png`, dataBase64: png })
    const lines = data.list.map(item => {
      const first = item.series[0]?.count ?? 0
      const last = item.series[item.series.length - 1]?.count ?? 0
      return `${item.tag}：${first} → ${last} 本`
    })
    return { sent: true, title, data: lines, message: `已把趋势图发给作者（${title}）。图里的数据：${lines.join('；')}` }
  },
  list_inspirations: async request => {
    const { listLocalInspirations } = await import('@/storage/local-inspiration')
    const list = listLocalInspirations({ keyword: text(request.request.keyword) || undefined, tag: text(request.request.tag) || undefined })
    const limit = Math.max(1, Math.min(Number(request.request.limit || 15), 40))
    return {
      total: list.length,
      items: list.slice(0, limit).map(item => ({ id: item.id, content: item.content, tag: item.tag || '', createTime: item.createTime || '' })),
    }
  },
  add_inspiration: async request => {
    const content = text(request.request.content)
    if (!content) return toolError('invalid_arguments', '灵感内容为空，没有保存。')
    const { addLocalInspiration } = await import('@/storage/local-inspiration')
    const saved = await addLocalInspiration({ content, tag: text(request.request.tag) || undefined })
    return { saved: true, inspiration: { id: saved.id, content: saved.content, tag: saved.tag || '' } }
  },
  generate_cover: async request => {
    const { generateCoverForAgent } = await import('./cover-image')
    return generateCoverForAgent(request)
  },
  writing_stats: async request => {
    const { getStatsOverview, getStatsStreak, getStatsTargets, getStatsTrend } = await import('@/storage/local-write-stats')
    const bookId = text(request.request.book_id) || undefined
    const days = Math.max(1, Math.min(Number(request.request.days || 7), 90))
    const trend = getStatsTrend(days, undefined, bookId)
    return {
      today: getStatsOverview(undefined, bookId),
      trend: { days: trend.days, startDate: trend.startDate, endDate: trend.endDate, totalWords: trend.totalWords, totalManualWords: trend.totalManualWords, totalAiWords: trend.totalAiWords, list: trend.list },
      streakDays: getStatsStreak(),
      targets: getStatsTargets(),
      bookId: bookId || '',
    }
  },
}

const failure = (tool: WebviewTool, error: unknown) =>
  RANK_TOOLS.has(tool)
    ? { status: 'refresh_failed', freshnessSatisfied: false, error: { code: 'service_error', message: String(error) } }
    : toolError('service_error', String(error))

export async function installWebviewExecutor(): Promise<() => void> {
  const clientId = crypto.randomUUID()
  const active = new Map<string, AbortController>()
  const stopRequest = await listen<WebviewExecutorRequest>(WEBVIEW_REQUEST_EVENT, event => {
    const request = event.payload
    if (request.clientId !== clientId || active.has(request.id)) return
    const controller = new AbortController()
    active.set(request.id, controller)
    void (async () => {
      try {
        const handler = HANDLERS[request.tool]
        if (!handler) throw new Error(`网页层没有这个工具：${request.tool}`)
        const result = await handler(request, controller.signal, async progress => {
          if (controller.signal.aborted) return
          const accepted = await invoke<boolean>('webview_executor_progress', { clientId, id: request.id, ...progress })
          if (!accepted) controller.abort()
        })
        if (!controller.signal.aborted) await invoke('webview_executor_complete', { clientId, id: request.id, result })
      } catch (error) {
        if (!controller.signal.aborted) {
          controller.abort()
          await invoke('webview_executor_complete', { clientId, id: request.id, result: failure(request.tool, error) })
        }
      } finally {
        active.delete(request.id)
      }
    })().catch(error => console.error('回传网页桥结果失败', error))
  })
  let stopCancel: (() => void) | undefined
  const dispose = () => {
    for (const controller of active.values()) controller.abort()
    active.clear()
    stopRequest()
    stopCancel?.()
  }
  try {
    stopCancel = await listen<{ id: string; clientId: string }>(WEBVIEW_CANCEL_EVENT, event => {
      if (event.payload.clientId === clientId) active.get(event.payload.id)?.abort()
    })
    await invoke('webview_executor_ready', { clientId })
  } catch (error) {
    dispose()
    throw error
  }
  return dispose
}
