/** 可读字符占比：汉字、ASCII 可见字符、常见中英标点、换行制表算可读，用来给解码候选打分 */
const readableRatio = (text: string): number => {
  const sample = Array.from(text.slice(0, 8000))
  if (!sample.length) return 0
  let ok = 0
  for (const ch of sample) {
    const code = ch.codePointAt(0) ?? 0
    if (
      code === 0x09 ||
      code === 0x0a ||
      code === 0x0d ||
      (code >= 0x20 && code <= 0x7e) ||
      (code >= 0x2000 && code <= 0x206f) ||
      (code >= 0x3000 && code <= 0x303f) ||
      (code >= 0x4e00 && code <= 0x9fff) ||
      (code >= 0xff00 && code <= 0xffef)
    )
      ok++
  }
  return ok / sample.length
}

/** 去掉零字节和其它不可见控制字符（保留换行、回车、制表），这类字符常见于站点下载或软件导出的 TXT 尾部填充 */
export const stripControlChars = (text: string): string =>
  // eslint-disable-next-line no-control-regex
  text.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, '')

/**
 * 优先按 BOM 解码；无 BOM 时先验证 UTF-8，再兼容 GBK/GB18030 中文文本，
 * 两者都不行或解出零字符时再按无 BOM 的 UTF-16 试一次。
 */
export const readLocalTxtContent = async (
  file: File,
  progress?: (read: number, total: number) => void
): Promise<string> => {
  const bytes = new Uint8Array(await file.slice(0, 3).arrayBuffer())
  const decode = async (encoding: string): Promise<string | null> => {
    try {
      const decoder = new TextDecoder(encoding, { fatal: true })
      const pieces: string[] = []
      for (let offset = 0; offset < file.size; offset += 512 * 1024) {
        const end = Math.min(file.size, offset + 512 * 1024)
        pieces.push(decoder.decode(await file.slice(offset, end).arrayBuffer(), { stream: true }))
        progress?.(end, file.size)
      }
      pieces.push(decoder.decode())
      return pieces.join('')
    } catch {
      return null
    }
  }
  let bomEncoding: string | null = null
  if (bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) bomEncoding = 'utf-8'
  else if (bytes[0] === 0xff && bytes[1] === 0xfe) bomEncoding = 'utf-16le'
  else if (bytes[0] === 0xfe && bytes[1] === 0xff) bomEncoding = 'utf-16be'

  let text: string | null
  if (bomEncoding) {
    text = await decode(bomEncoding)
  } else {
    text = (await decode('utf-8')) ?? (await decode('gb18030'))
    // 没有 BOM 的 UTF-16 文件：GB18030 要么直接解码失败，要么解成带零字符的乱码
    if (text == null || text.includes('\0')) {
      for (const encoding of ['utf-16le', 'utf-16be']) {
        const candidate = await decode(encoding)
        if (candidate && !candidate.includes('\0') && readableRatio(candidate) >= 0.9)
          return stripControlChars(candidate)
      }
    }
  }
  if (text == null) {
    throw new Error('无法正确读取 TXT 编码，请将原文件另存为 UTF-8 后重新导入')
  }

  // 正常文本里夹了零字节（下载站填充、软件导出残留）直接剔掉即可，不必让用户去转码；
  // 剔掉的比例过半或剩下的不像文字，才当作损坏/二进制文件拒绝
  const cleaned = stripControlChars(text)
  const mostlyGarbage = cleaned.length < text.length * 0.5 || !cleaned.trim() || readableRatio(cleaned) < 0.6
  if (text.includes('\0') && mostlyGarbage) {
    throw new Error('文件包含非文本字符，请将原文件另存为 UTF-8 格式的 TXT 后重新导入')
  }
  return cleaned
}
