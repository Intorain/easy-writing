import type { NovelRankItem } from '@/types/novel-rank'
import { buildQidianDigitMapFromTtf, decodeQidianObfuscatedNumber } from '@/utils/qidian-font'
import { crawlRankPageViaWindow } from '@/utils/local-rank-window'
import { throwIfAborted } from '@/utils/abortable'
import { isTauriRuntime } from '@/storage'

/**
 * 起点榜单适配器（移植自老服务端 statistics/service/rank/qidian.ts，cheerio → DOMParser）。
 *
 * 起点全站有 probe.js 反爬，普通请求只能拿到探针拦截页——
 * 抓取走隐藏窗口（local-rank-window）：真实浏览器环境自然通过探针，
 * cookie 由窗口会话自持。月票数字是"反爬字体"混淆，下载页面引用的 ttf
 * 解 cmap+post 表得到 codepoint→数字 映射后还原。
 */

const QIDIAN_RANK_SELECTOR = '#rank-view-list .book-img-text ul li'
const QIDIAN_FONT_HOST = 'qdfepccdn.qidian.com'
const PAGE_GAP_MS = () => 900 + Math.random() * 600

const normalizeSpaces = (text: string) => String(text || '').replace(/\s+/g, ' ').trim()

const stripPrefix = (text: string, prefix: string) => {
  const trimmed = normalizeSpaces(text)
  return trimmed.startsWith(prefix) ? trimmed.slice(prefix.length).trim() : trimmed
}

const detectQidianBaseHost = (html: string) =>
  html.includes('www.qdmm.com') || html.includes('起点女生网') ? 'www.qdmm.com' : 'www.qidian.com'

const toAbsoluteHttps = (url: string | null | undefined, baseHost: string) => {
  const value = String(url || '').trim()
  if (!value) return ''
  if (value.startsWith('http://') || value.startsWith('https://')) return value
  if (value.startsWith('//')) return `https:${value}`
  if (value.startsWith('/')) return `https://${baseHost}${value}`
  return `https://${value}`
}

/** 从页面里抠出反爬字体的 ttf 地址（@font-face 的 truetype 源） */
export const extractQidianFontTtfUrl = (html: string) => {
  const match = String(html || '').match(/url\(['"]([^'"]+\.ttf[^'"]*)['"]\)\s*format\(['"]truetype['"]\)/i)
  if (!match?.[1]) return ''
  const url = match[1].replace(/&amp;/g, '&').trim()
  if (url.startsWith('//')) return `https:${url}`
  if (url.startsWith('https://')) return url
  return ''
}

/** 起点榜单翻页：/rank/yuepiao/ → /rank/yuepiao/page2/（我们内置的源都是裸榜单路径） */
export const buildQidianPagedUrl = (baseUrl: string, pageNo: number) => {
  if (pageNo <= 1) return baseUrl
  const url = new URL(baseUrl)
  const parts = url.pathname.split('/').filter(Boolean)
  const last = parts[parts.length - 1] || ''
  if (/^page\d+$/i.test(last)) parts[parts.length - 1] = `page${pageNo}`
  else parts.push(`page${pageNo}`)
  url.pathname = `/${parts.join('/')}/`
  return url.toString()
}

export interface QidianRankParsed {
  items: NovelRankItem[]
  fontTtfUrl: string
  /** 月票 span 里出现的全部混淆 codepoint（去重），用于按需解码字体 */
  metricCodepoints: number[]
}

/** 解析起点榜单页；metric 文本先保留混淆原文，拿到字体映射后再统一解码 */
export const parseQidianRankHtml = (html: string): QidianRankParsed => {
  const doc = new DOMParser().parseFromString(String(html || ''), 'text/html')
  const baseHost = detectQidianBaseHost(html)
  const items: NovelRankItem[] = []
  const codepoints = new Set<number>()

  doc.querySelectorAll(QIDIAN_RANK_SELECTOR).forEach((el, index) => {
    const rankNo = Number(el.getAttribute('data-rid')) || index + 1

    const bookAnchor = el.querySelector('.book-mid-info h2 a')
    const coverAnchor = el.querySelector('.book-img-box a')
    const bookUrl = toAbsoluteHttps(bookAnchor?.getAttribute('href') || coverAnchor?.getAttribute('href'), baseHost)
    const bookTitle = normalizeSpaces(bookAnchor?.textContent || '')
    if (!bookTitle || !bookUrl) return
    const bookId =
      String(bookAnchor?.getAttribute('data-bid') || coverAnchor?.getAttribute('data-bid') || '').trim() ||
      bookUrl.match(/\/book\/(\d+)/)?.[1] ||
      null

    const coverUrl = toAbsoluteHttps(el.querySelector('.book-img-box img')?.getAttribute('src'), baseHost) || null

    const authorLinks = Array.from(el.querySelectorAll('.book-mid-info p.author a'))
    const authorName = normalizeSpaces(authorLinks[0]?.textContent || '') || null
    const categoryName = normalizeSpaces(authorLinks[1]?.textContent || '') || null
    const subCategoryName = normalizeSpaces(authorLinks[2]?.textContent || '') || null
    const statusText =
      normalizeSpaces(Array.from(el.querySelectorAll('.book-mid-info p.author span')).pop()?.textContent || '') || null

    const updateAnchor = el.querySelector('.book-mid-info p.update a')
    const lastChapterTitle = normalizeSpaces(stripPrefix(updateAnchor?.textContent || '', '最新更新')) || null
    const lastChapterUrl = toAbsoluteHttps(updateAnchor?.getAttribute('href'), baseHost) || null
    const lastUpdateTimeText = normalizeSpaces(el.querySelector('.book-mid-info p.update span')?.textContent || '') || null

    // 月票数字（混淆字体）：先存原文，收集 codepoint 待解码
    const metricEncoded = normalizeSpaces(el.querySelector('.book-right-info .total span[class]')?.textContent || '')
    for (const char of metricEncoded) {
      const cp = char.codePointAt(0)
      if (cp) codepoints.add(cp)
    }

    items.push({
      rankNo,
      rankChange: 0,
      bookTitle,
      bookId,
      bookUrl,
      coverUrl,
      authorName,
      categoryName,
      categorySubName: subCategoryName,
      statusText,
      intro: normalizeSpaces(el.querySelector('.book-mid-info p.intro')?.textContent || '') || null,
      readingText: null,
      readingCount: 0,
      metricName: '月票',
      metricText: metricEncoded || null,
      metricValue: 0,
      lastChapterTitle,
      lastChapterUrl,
      lastUpdateTimeText,
    })
  })

  return {
    items,
    fontTtfUrl: extractQidianFontTtfUrl(html),
    metricCodepoints: [...codepoints],
  }
}

/** 用字体映射把条目里的月票原文解码成数字（映射为空则清掉混淆原文，不展示乱码） */
export const applyQidianDigitMap = (items: NovelRankItem[], digitMap: Map<number, string>): NovelRankItem[] =>
  items.map(item => {
    if (!item.metricText) return item
    if (!digitMap.size) return { ...item, metricText: null, metricValue: 0 }
    const decoded = decodeQidianObfuscatedNumber(item.metricText, digitMap)
    return {
      ...item,
      metricText: decoded.value > 0 ? decoded.text : null,
      metricValue: decoded.value,
    }
  })

const fetchQidianFontDigitMap = async (fontTtfUrl: string, codepoints: number[], signal?: AbortSignal): Promise<Map<number, string>> => {
  throwIfAborted(signal)
  if (!fontTtfUrl || !codepoints.length || !isTauriRuntime()) return new Map()
  let parsed: URL
  try {
    parsed = new URL(fontTtfUrl)
  } catch {
    return new Map()
  }
  // 只信起点自家 CDN 的字体，防页面内容把我们导去别处
  if (parsed.protocol !== 'https:' || parsed.hostname.toLowerCase() !== QIDIAN_FONT_HOST) return new Map()
  try {
    const { fetch: tauriFetch } = await import('@tauri-apps/plugin-http')
    const response = await tauriFetch(fontTtfUrl, {
      method: 'GET',
      headers: { Referer: 'https://www.qidian.com/' },
      signal,
    })
    if (!response.ok) return new Map()
    return buildQidianDigitMapFromTtf(await response.arrayBuffer(), codepoints)
  } catch {
    throwIfAborted(signal)
    // 字体拿不到只损失月票数字，条目本身照常入库
    return new Map()
  }
}

/** 抓取一个起点榜单源：逐页隐藏窗口取渲染后 HTML，合并去重，月票按需解码 */
export const fetchQidianRank = async (baseUrl: string, maxPages: number, signal?: AbortSignal): Promise<NovelRankItem[]> => {
  const pages = Math.max(1, Math.min(Number(maxPages) || 1, 5))
  const merged: NovelRankItem[] = []
  const seen = new Set<string>()
  let fontTtfUrl = ''
  const codepoints = new Set<number>()

  for (let page = 1; page <= pages; page += 1) {
    throwIfAborted(signal)
    const url = buildQidianPagedUrl(baseUrl, page)
    let html = ''
    try {
      html = await crawlRankPageViaWindow(url, { waitSelector: QIDIAN_RANK_SELECTOR, scrollRounds: 0 }, signal)
    } catch (error) {
      throwIfAborted(signal)
      // 首页失败整体报错；后续页失败用已拿到的部分
      if (page === 1) throw error instanceof Error ? error : new Error(String(error))
      break
    }
    const parsed = parseQidianRankHtml(html)
    if (!parsed.items.length) {
      if (page === 1) throw new Error('起点榜单页没有解析到条目（可能被限制或页面结构变更）')
      break
    }
    if (!fontTtfUrl && parsed.fontTtfUrl) fontTtfUrl = parsed.fontTtfUrl
    parsed.metricCodepoints.forEach(cp => codepoints.add(cp))
    for (const item of parsed.items) {
      const key = item.bookId || item.bookUrl
      if (seen.has(key)) continue
      seen.add(key)
      merged.push({ ...item, rankNo: merged.length + 1 })
    }
    if (page < pages) await new Promise(resolve => setTimeout(resolve, PAGE_GAP_MS()))
  }

  if (!merged.length) throw new Error('起点榜单没有抓到条目')
  const digitMap = await fetchQidianFontDigitMap(fontTtfUrl, [...codepoints], signal)
  return applyQidianDigitMap(merged, digitMap)
}
