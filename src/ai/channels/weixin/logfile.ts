import { isTauriRuntime } from '@/storage/writing-storage'

/**
 * 微信助手·技术日志（排障用的那份）。
 *
 * 一行一事件，追加写进应用数据目录 logs/weixin-bot.log：连接状态变化、收到消息、走了哪条路、
 * 每次工具调用、模型循环结果、事实核对替换、发送成败、主动推送去向。作者报障时把这个文件发回来即可。
 *
 * 写法照搬写作取证日志（storage/write-journal）：内存攒批（3 秒或 20 行）再落盘，错误级别立刻落盘；
 * 文件超过 1MB 轮转一份 .1，只留两代。网页端没有文件系统，微信助手也不在网页端跑，直接不写。
 * 日志只是辅助能力：任何写入失败都不能影响通道本身。
 */

export type WeixinLogLevel = 'info' | 'warn' | 'error'

const FLUSH_INTERVAL_MS = 3000
const FLUSH_BATCH_SIZE = 20
const MAX_FILE_BYTES = 1024 * 1024
/** 日志里原文最多留多少字：够看清说了什么，又不会把文件撑爆 */
const TEXT_PREVIEW_CHARS = 200
export const WEIXIN_LOG_FILE_NAME = 'weixin-bot.log'

let buffer: string[] = []
let flushTimer: ReturnType<typeof setTimeout> | null = null
let flushing: Promise<void> | null = null
let logDirPromise: Promise<string> | null = null
let warnedOnce = false

/** 把原文压成一行预览：换行折成空格，超长截断 */
export const previewText = (value: unknown, max = TEXT_PREVIEW_CHARS) => {
  const text = String(value ?? '').replace(/\s+/g, ' ').trim()
  return text.length > max ? `${text.slice(0, max)}…` : text
}

const formatLine = (level: WeixinLogLevel, event: string, detail?: Record<string, unknown>) => {
  const time = new Date().toISOString()
  const tag = level.toUpperCase().padEnd(5)
  if (!detail || !Object.keys(detail).length) return `${time} ${tag} ${event}`
  let payload = ''
  try {
    payload = JSON.stringify(detail)
  } catch {
    payload = String(detail)
  }
  return `${time} ${tag} ${event} ${payload}`
}

const resolveLogDir = async () => {
  if (!logDirPromise) {
    logDirPromise = (async () => {
      const { appDataDir, join } = await import('@tauri-apps/api/path')
      return join(await appDataDir(), 'logs')
    })().catch(error => {
      logDirPromise = null
      throw error
    })
  }
  return await logDirPromise
}

const writeLines = async (lines: string[]) => {
  const [{ invoke }, { join }] = await Promise.all([import('@tauri-apps/api/core'), import('@tauri-apps/api/path')])
  await invoke('append_app_log', {
    filePath: await join(await resolveLogDir(), WEIXIN_LOG_FILE_NAME),
    content: `${lines.join('\n')}\n`,
    maxBytes: MAX_FILE_BYTES,
  })
}

const flush = async () => {
  if (flushTimer) {
    clearTimeout(flushTimer)
    flushTimer = null
  }
  if (!buffer.length) return
  const lines = buffer
  buffer = []
  // 串行落盘，避免两批并发追加把行序写乱
  const previous = flushing || Promise.resolve()
  flushing = previous
    .then(() => writeLines(lines))
    .catch(error => {
      if (!warnedOnce) {
        warnedOnce = true
        console.warn('[微信助手] 技术日志写入失败，后续不再提示', error)
      }
    })
  await flushing
}

const scheduleFlush = () => {
  if (flushTimer) return
  flushTimer = setTimeout(() => void flush(), FLUSH_INTERVAL_MS)
}

/** 记一行；网页端静默跳过 */
export const weixinLog = (level: WeixinLogLevel, event: string, detail?: Record<string, unknown>) => {
  if (!isTauriRuntime()) return
  buffer.push(formatLine(level, event, detail))
  if (level === 'error' || buffer.length >= FLUSH_BATCH_SIZE) {
    void flush()
  } else {
    scheduleFlush()
  }
}

/** 立即把攒着的写完：关通道、关窗口前调用 */
export const flushWeixinLogFile = () => flush()

// 页面重载（开发版热更新、刷新）前尽量把缓冲写掉；写不完的那几行由 Rust 侧的 host- 日志兜底
if (typeof window !== 'undefined' && isTauriRuntime()) {
  window.addEventListener('pagehide', () => {
    void flush()
  })
}

/** 日志所在目录（桌面端）；设置页「打开日志文件夹」用 */
export const getWeixinLogDir = async () => {
  if (!isTauriRuntime()) throw new Error('技术日志只在桌面客户端可用')
  return resolveLogDir()
}

/** 用系统文件管理器打开日志目录；先把缓冲写完，作者打开就能看到最新几行 */
export const openWeixinLogDir = async () => {
  await flush()
  const dir = await getWeixinLogDir()
  const { invoke } = await import('@tauri-apps/api/core')
  // 复用备份目录的打开命令：它只做"确保目录存在 + 系统打开"，与备份无关
  await invoke('open_backup_dir', { path: dir })
  return dir
}
