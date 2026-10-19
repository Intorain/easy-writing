import { computed, onBeforeUnmount, onMounted, ref, watch, type Ref } from 'vue'

/**
 * 打字机模式：打字时光标一旦低于基准线（编辑区高度 × 设定比例），就把编辑区上滚到光标回到基准线。
 * 只在键盘输入后触发，鼠标点选、拖选、输入法候选未上屏时不动。写到章末时下面没有内容可滚，
 * 所以开着打字机模式时给正文底部补一段空白（基准线以下那一截的高度），最后一行也能停在基准线上。
 */

export interface TypewriterGeometry {
  /** 光标底边（视口坐标） */
  caretBottom: number
  /** 滚动容器顶边（视口坐标）与可见高度 */
  containerTop: number
  containerHeight: number
  /** 基准线占容器高度的比例，0–1 */
  ratio: number
}

/** 需要上滚多少像素；光标还在基准线之上返回 0 */
export const typewriterScrollDelta = ({ caretBottom, containerTop, containerHeight, ratio }: TypewriterGeometry) => {
  if (!(containerHeight > 0)) return 0
  const line = containerTop + containerHeight * Math.min(Math.max(ratio, 0.1), 0.9)
  return caretBottom > line ? Math.round(caretBottom - line) : 0
}

/** 正文底部要补的空白：基准线以下那一截 */
export const typewriterBottomGap = (containerHeight: number, ratio: number) =>
  containerHeight > 0 ? Math.max(0, Math.round(containerHeight * (1 - Math.min(Math.max(ratio, 0.1), 0.9)))) : 0

interface CaretEditorLike {
  state: { selection: { head: number } }
  view: { coordsAtPos: (pos: number) => { top: number; bottom: number } }
}

interface UseTypewriterScrollOptions {
  editor: Ref<CaretEditorLike | undefined>
  scrollRef: Ref<HTMLElement | null>
  enabled: Ref<boolean>
  /** 百分数，如 50 */
  ratio: Ref<number>
  composing: Ref<boolean>
}

export const useTypewriterScroll = (options: UseTypewriterScrollOptions) => {
  const { editor, scrollRef, enabled, ratio, composing } = options
  const containerHeight = ref(0)
  let frame = 0
  let observer: ResizeObserver | null = null

  const measure = () => {
    containerHeight.value = scrollRef.value?.clientHeight || 0
  }

  const fraction = computed(() => (Number(ratio.value) > 0 ? Number(ratio.value) / 100 : 0.5))

  /** 开着打字机模式时正文底部补的空白，交给容器的样式变量 */
  const typewriterStyle = computed<Record<string, string>>(() => {
    const style: Record<string, string> = {}
    if (enabled.value) style['--typewriter-gap'] = `${typewriterBottomGap(containerHeight.value, fraction.value)}px`
    return style
  })

  const adjust = () => {
    const ed = editor.value
    const el = scrollRef.value
    if (!ed || !el || !enabled.value || composing.value) return
    let coords: { top: number; bottom: number }
    try {
      coords = ed.view.coordsAtPos(ed.state.selection.head)
    } catch {
      return
    }
    const rect = el.getBoundingClientRect()
    const delta = typewriterScrollDelta({ caretBottom: coords.bottom, containerTop: rect.top, containerHeight: rect.height, ratio: fraction.value })
    if (delta <= 0) return
    const max = Math.max(0, el.scrollHeight - el.clientHeight)
    el.scrollTop = Math.min(el.scrollTop + delta, max)
  }

  /** 键盘输入落定后调用：等本帧布局完成再量光标 */
  const onTyped = () => {
    if (!enabled.value || typeof window === 'undefined') return
    if (frame) window.cancelAnimationFrame(frame)
    frame = window.requestAnimationFrame(() => {
      frame = 0
      adjust()
    })
  }

  watch(scrollRef, el => {
    observer?.disconnect()
    observer = null
    if (el && typeof ResizeObserver !== 'undefined') {
      observer = new ResizeObserver(measure)
      observer.observe(el)
    }
    measure()
  })
  watch(enabled, on => {
    if (on) {
      measure()
      onTyped()
    }
  })

  onMounted(measure)
  onBeforeUnmount(() => {
    observer?.disconnect()
    if (frame && typeof window !== 'undefined') window.cancelAnimationFrame(frame)
  })

  return { typewriterStyle, onTyped, containerHeight }
}
