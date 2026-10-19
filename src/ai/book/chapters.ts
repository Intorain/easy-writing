import { getLocalBookTreeData, getLocalChapterDetailData } from '@/storage/local-reference'

/** 作品章节的阅读顺序视图：工具、抽取、校验都靠它把章节 id 换成"第几章" */
export interface ChapterEntry {
  id: number
  title: string
  summary: string
  volumeTitle: string
  /** 全书顺序号（从 1 起），与章节标题里的数字无关 */
  order: number
}

export const loadBookChapters = async (bookId: string | number): Promise<ChapterEntry[]> => {
  const { data: volumes } = await getLocalBookTreeData({ bookId })
  const list: ChapterEntry[] = []
  for (const volume of volumes) {
    for (const chapter of volume.children || []) {
      list.push({
        id: chapter.id,
        title: chapter.title,
        summary: String(chapter.summary || ''),
        volumeTitle: volume.title,
        order: list.length + 1,
      })
    }
  }
  return list
}

export const readBookChapterText = async (bookId: string | number, chapterId: number) => {
  const { data } = await getLocalChapterDetailData({ bookId, id: chapterId })
  return String(data?.textContent || '')
}

export const chapterLabel = (chapter: ChapterEntry) => `#${chapter.order}《${chapter.title}》(id:${chapter.id})`
