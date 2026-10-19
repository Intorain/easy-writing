import { isTauriRuntime } from '@/storage'
import { abortable, throwIfAborted } from '@/utils/abortable'

/**
 * 榜单隐藏抓取窗口的前端封装。
 *
 * 桌面端调 Rust 命令 rank_crawl_render_page：开一个不可见的真实浏览器窗口
 * 加载榜单页（起点 probe.js 反爬自然通过、cookie 由窗口会话自持），
 * 等目标选择器出现、按需滚动触发懒加载，拿回渲染后的完整 HTML。
 * 网页端没有窗口能力，给可读提示。
 */

export interface RankRenderCrawlOptions {
  /** 页面就绪的标志选择器（出现即认为内容已渲染） */
  waitSelector: string
  /** 懒加载滚动轮数；0 = 选择器出现即取（起点分页站不需要滚动） */
  scrollRounds?: number
  timeoutMs?: number
}

export const crawlRankPageViaWindow = async (url: string, options: RankRenderCrawlOptions, signal?: AbortSignal): Promise<string> => {
  throwIfAborted(signal)
  if (!isTauriRuntime()) {
    throw new Error('榜单抓取是桌面版功能：浏览器受跨域限制无法直接访问平台站点，请在桌面客户端使用')
  }
  const { invoke } = await import('@tauri-apps/api/core')
  const html = await abortable(invoke<string>('rank_crawl_render_page', {
    request: {
      url,
      waitSelector: options.waitSelector,
      scrollRounds: Math.max(0, Math.floor(options.scrollRounds || 0)),
      timeoutMs: options.timeoutMs,
    },
  }), signal)
  if (!String(html || '').trim()) throw new Error('抓取窗口没有返回页面内容')
  return html
}
