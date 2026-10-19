import { agentCommand } from './client'
import { safeFileName } from './media'

/**
 * 按封面方案出图（网页桥工具 generate_cover 的执行部分）。
 * 宿主已核对方案、作品与图片模型并经作者确认；这里调图片模型、裁成封面比例、挂到方案与作品，微信里再把图发过去。
 * 密钥由本地模型库自己读，不经宿主；成图不回传宿主（网页桥结果有大小上限），只回一句结论。
 */

/** 画面中不要出现的东西，与封面工具一致 */
const NEGATIVE_PROMPT = '水印, watermark, logo, 标志, 签名, frame, lowres, blurry, deformed, bad anatomy, extra fingers'
/** 封面比例与封面工具的预览一致（225:400） */
const COVER_RATIO = 400 / 225

export interface CoverJob {
  artifactId: string
  prompt: string
  style: string
  bookId: string
  bookTitle: string
  regenerate?: boolean
  modelCode: string
  modelName: string
  size: string
}

export interface CoverDeps {
  /** 调图片模型，返回图片地址（blob: 或远端地址）与实际用的模型名 */
  generate: (job: CoverJob, prompt: string) => Promise<{ imageUrl: string; modelName: string }>
  /** 裁成封面比例，返回 PNG data URL */
  compose: (imageUrl: string) => Promise<string>
  attach: (artifactId: string, coverUrl: string) => Promise<void>
  updateBook: (bookId: number, coverUrl: string) => Promise<void>
  send: (media: { kind: 'image'; fileName: string; dataBase64: string }) => Promise<void>
}

/** 本地书编号是负数；0 或非数字表示方案没有关联作品 */
export const bookNumber = (bookId: unknown): number | undefined => {
  const value = Number(bookId)
  return Number.isFinite(value) && value !== 0 ? value : undefined
}

/** 与封面工具一样把风格并进描述；描述里已写了风格就不重复 */
export const composeCoverPrompt = (prompt: string, style: string) => {
  const base = String(prompt || '').trim()
  const label = String(style || '').trim()
  if (!base) return ''
  return label && !base.includes(label) ? `${base}，${label}风格` : base
}

const loadImage = (url: string) =>
  new Promise<HTMLImageElement>((resolve, reject) => {
    const image = new Image()
    image.crossOrigin = 'anonymous'
    image.onload = () => resolve(image)
    image.onerror = () => reject(new Error('封面图片读取失败'))
    image.src = url
  })

/** 把生成的图裁成封面比例（居中铺满），输出 PNG；不排书名，纯画面 */
export const composeCoverDataUrl = async (imageUrl: string): Promise<string> => {
  const { getImageObjectSource } = await import('@/utils/image-proxy')
  const source = await getImageObjectSource(imageUrl)
  try {
    const image = await loadImage(source.url)
    const width = image.naturalWidth || 768
    const height = Math.round(width * COVER_RATIO)
    const canvas = document.createElement('canvas')
    canvas.width = width
    canvas.height = height
    const ctx = canvas.getContext('2d')
    if (!ctx) throw new Error('封面合成失败')
    const scale = Math.max(width / (image.naturalWidth || width), height / (image.naturalHeight || height))
    const drawWidth = (image.naturalWidth || width) * scale
    const drawHeight = (image.naturalHeight || height) * scale
    ctx.drawImage(image, (width - drawWidth) / 2, (height - drawHeight) / 2, drawWidth, drawHeight)
    return canvas.toDataURL('image/png')
  } finally {
    if (source.revoke && source.url.startsWith('blob:')) {
      try {
        URL.revokeObjectURL(source.url)
      } catch {
        // 释放失败不影响结果
      }
    }
  }
}

const defaultDeps = (): CoverDeps => ({
  generate: async (job, prompt) => {
    const { generateLocalAiImage } = await import('@/storage/local-ai-images')
    const { data } = await generateLocalAiImage({
      scene: 'cover',
      bookId: bookNumber(job.bookId),
      title: job.bookTitle || '封面',
      prompt,
      negativePrompt: NEGATIVE_PROMPT,
      style: job.style || undefined,
      size: job.size,
      model: job.modelCode || undefined,
    })
    return { imageUrl: data?.imageUrl || '', modelName: data?.modelName || job.modelName }
  },
  compose: composeCoverDataUrl,
  attach: async (artifactId, coverUrl) => {
    await agentCommand('attach_cover', { id: artifactId, coverUrl })
  },
  updateBook: async (bookId, coverUrl) => {
    const { updateLocalBookData } = await import('@/storage/local-book-bridge')
    await updateLocalBookData({ id: bookId, coverUrl })
  },
  send: async media => {
    const { sendWeixinMedia } = await import('@/ai/channels/weixin/channel')
    await sendWeixinMedia(media)
  },
})

export const generateCoverForAgent = async (
  request: { request: Record<string, unknown>; channel?: string },
  deps: CoverDeps = defaultDeps(),
): Promise<Record<string, unknown>> => {
  const job = request.request as unknown as CoverJob
  const prompt = composeCoverPrompt(job.prompt, job.style)
  if (!prompt) return { ok: false, toolError: { code: 'empty_prompt', retryable: false, message: '封面方案没有画面描述。' } }
  const generated = await deps.generate(job, prompt)
  if (!generated.imageUrl) throw new Error('图片模型没有返回图片')
  const coverUrl = await deps.compose(generated.imageUrl)
  await deps.attach(job.artifactId, coverUrl)
  const book = bookNumber(job.bookId)
  let savedToBook = false
  let bookNote = '方案没有关联作品，没有写进作品封面'
  if (book !== undefined) {
    try {
      await deps.updateBook(book, coverUrl)
      savedToBook = true
      bookNote = '已保存为作品封面'
    } catch (error) {
      bookNote = `写进作品封面失败（${error instanceof Error ? error.message : String(error)}），作者可在成果面板重试`
    }
  }
  let sent = false
  if (request.channel === 'weixin') {
    await deps.send({
      kind: 'image',
      fileName: `${safeFileName(job.bookTitle || '封面')}-封面.png`,
      dataBase64: coverUrl.slice(coverUrl.indexOf(',') + 1),
    })
    sent = true
  }
  const tail = sent ? '图在这条消息上方' : '作者可在成果面板查看'
  return {
    generated: true,
    artifactId: job.artifactId,
    modelName: generated.modelName,
    size: job.size,
    savedToBook,
    sent,
    message: `封面已用「${generated.modelName}」生成，已挂到封面方案，${bookNote}；${tail}。`,
  }
}
