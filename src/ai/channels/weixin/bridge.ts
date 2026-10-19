import { desktopInvoke } from '@/storage/desktop-records'

/**
 * 微信 ClawBot 通道与 Rust 侧（src-tauri/src/weixin_bot.rs）的桥：
 * 命令调用与事件监听都收在这里，通道层和设置页只认这组函数与类型。
 */

export type WeixinBotState = 'offline' | 'connecting' | 'online' | 'stale_token' | 'error'

/** 发出去的是什么：text 普通回复 / push 主动推送 / image、file 媒体 */
export type WeixinSendKind = 'text' | 'push' | 'image' | 'file'

export interface WeixinSendRecord {
  atMs: number
  ok: boolean
  kind: WeixinSendKind
  /** 文本按字符数，媒体按明文字节数 */
  chars: number
  clientId: string
  error: string
}

export interface WeixinBotStatus {
  bound: boolean
  state: WeixinBotState
  userId: string
  botId: string
  boundAtMs: number
  lastMessageAtMs: number
  lastError: string
  protocolVersion: string
  pendingCount: number
  recentSends: WeixinSendRecord[]
  /** 是否拿到过会话令牌：拿到过才能主动推送 */
  pushAvailable: boolean
  lastContextAtMs: number
}

export type WeixinInboundKind = 'text' | 'voice' | 'image' | 'file' | 'video' | 'other'

export interface WeixinInboundMessage {
  /** 微信的消息编号超出 JS 安全整数范围，按字符串传递 */
  messageId: string
  fromUserId: string
  toUserId: string
  /** 回复时原样带回 */
  contextToken: string
  createTimeMs: number
  kind: WeixinInboundKind
  /** 文本或语音转写；语音未转写出文字时为空 */
  text: string
}

export interface WeixinLoginStart {
  qrcode: string
  qrcodeImgContent: string
}

export type WeixinLoginStatus =
  | 'wait'
  | 'scaned'
  | 'confirmed'
  | 'expired'
  | 'need_verifycode'
  | 'verify_code_blocked'
  | 'binded_redirect'

export interface WeixinLoginPoll {
  status: WeixinLoginStatus
  message: string
}

/** 发给微信的媒体：图片或文件，内容 base64（不带 data: 前缀） */
export interface WeixinMediaPayload {
  kind: 'image' | 'file'
  fileName: string
  dataBase64: string
}

export const WEIXIN_EVENT_MESSAGE = 'weixin-bot://message'
export const WEIXIN_EVENT_STATUS = 'weixin-bot://status'

export const weixinBotStatus = () => desktopInvoke<WeixinBotStatus>('weixin_bot_status')
export const weixinBotStart = () => desktopInvoke<WeixinBotStatus>('weixin_bot_start')
export const weixinBotAttach = () => desktopInvoke<WeixinInboundMessage[]>('weixin_bot_attach')
/** 这条消息已记进日志：Rust 侧不再在页面重载后补发 */
export const weixinBotAck = (messageId: string) => desktopInvoke<boolean>('weixin_bot_ack', { messageId })
export const weixinBotLogout = () => desktopInvoke<WeixinBotStatus>('weixin_bot_logout')
export const weixinBotLoginStart = () => desktopInvoke<WeixinLoginStart>('weixin_bot_login_start')
export const weixinBotLoginPoll = (verifyCode?: string) =>
  desktopInvoke<WeixinLoginPoll>('weixin_bot_login_poll', { verifyCode: verifyCode || null })
export const weixinBotLoginCancel = () => desktopInvoke<void>('weixin_bot_login_cancel')
export const weixinBotSendText = (toUserId: string, contextToken: string, text: string) =>
  desktopInvoke<WeixinSendRecord>('weixin_bot_send_text', { toUserId, contextToken, text })
export const weixinBotSendTyping = (contextToken: string, typing: boolean) =>
  desktopInvoke<void>('weixin_bot_send_typing', { contextToken, typing })
/** 用 Rust 侧记住的最近一次会话令牌主动发一条文本；没拿到过令牌时抛错 */
export const weixinBotPushText = (text: string) => desktopInvoke<WeixinSendRecord>('weixin_bot_push_text', { text })
/** 发图片或文件：Rust 侧完成 AES 加密、CDN 上传与消息发送 */
export const weixinBotSendMedia = (toUserId: string, contextToken: string, media: WeixinMediaPayload) =>
  desktopInvoke<WeixinSendRecord>('weixin_bot_send_media', {
    toUserId,
    contextToken,
    mediaKind: media.kind,
    fileName: media.fileName,
    dataBase64: media.dataBase64,
  })

type Unlisten = () => void

export const listenWeixinMessages = async (handler: (message: WeixinInboundMessage) => void): Promise<Unlisten> => {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<WeixinInboundMessage>(WEIXIN_EVENT_MESSAGE, event => handler(event.payload))
}

export const listenWeixinStatus = async (handler: (status: WeixinBotStatus) => void): Promise<Unlisten> => {
  const { listen } = await import('@tauri-apps/api/event')
  return listen<WeixinBotStatus>(WEIXIN_EVENT_STATUS, event => handler(event.payload))
}
