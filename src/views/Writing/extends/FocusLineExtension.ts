import { Extension } from '@tiptap/core'
import { Plugin, PluginKey } from 'prosemirror-state'
import { Decoration, DecorationSet } from 'prosemirror-view'

/**
 * 专注模式的"当前段"标记：给光标所在的文本块加一个类，其余段的变淡由样式按这个类反选。
 * 只记段落级（ProseMirror 不知道一段在屏幕上折成几行）；开关切换后由调用方派发一次空事务刷新装饰。
 */
export const FOCUS_LINE_CLASS = 'focus-current'
export const FOCUS_LINE_REFRESH = 'focusLineRefresh'

export interface FocusLineOptions {
  enabled: () => boolean
}

export default Extension.create<FocusLineOptions>({
  name: 'focusLine',

  addOptions() {
    return { enabled: () => false }
  },

  addProseMirrorPlugins() {
    const { enabled } = this.options
    return [
      new Plugin({
        key: new PluginKey('focusLine'),
        props: {
          decorations(state) {
            if (!enabled()) return DecorationSet.empty
            const $head = state.selection.$head
            // 光标所在的最内层块节点就是"当前段"；在文档顶层（没有段）时不标
            let depth = $head.depth
            while (depth > 0 && !$head.node(depth).isTextblock) depth -= 1
            if (depth === 0) return DecorationSet.empty
            const decoration = Decoration.node($head.before(depth), $head.after(depth), { class: FOCUS_LINE_CLASS })
            return DecorationSet.create(state.doc, [decoration])
          },
        },
      }),
    ]
  },
})
