import type { AgentReference } from '@/types/agent-creation'

/** 引用条目的图标：按类型区分，人物与设定再按资料种类区分 */
export function referenceIcon(reference: Pick<AgentReference, 'kind' | 'entityType'>) {
  switch (reference.kind) {
    case 'book':
      return 'fa-solid fa-book-open'
    case 'volume':
      return 'fa-solid fa-layer-group'
    case 'entity':
      return reference.entityType === 'world_setting' ? 'fa-solid fa-earth-asia' : 'fa-regular fa-user'
    case 'selection':
      return 'fa-solid fa-quote-left'
    case 'breakdown':
      return 'fa-solid fa-book-bookmark'
    case 'rank':
      return 'fa-solid fa-ranking-star'
    case 'material':
      return 'fa-regular fa-file'
    default:
      return 'fa-regular fa-file-lines'
  }
}
