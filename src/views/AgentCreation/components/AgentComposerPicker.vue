<template>
  <el-popover
    :visible="visible"
    :virtual-ref="input"
    virtual-triggering
    :placement="placement"
    strategy="fixed"
    :teleported="true"
    :popper-options="popperOptions"
    :popper-style="{ maxHeight: `${availableHeight}px` }"
    :width="availableWidth"
    :popper-class="`ac-floating ac-composer-picker${availableHeight < 320 ? ' is-compact' : ''}`"
    :persistent="false"
    :show-after="0"
    :hide-after="0"
  >
    <div ref="panel" class="ac-reference-picker">
      <div v-if="!booksOnly" class="ac-tabs" role="group" aria-label="选择内容类型">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          :class="{ active: category === tab.id }"
          :aria-pressed="category === tab.id"
          @mousedown.prevent
          @click="switchCategory(tab.id)"
        >
          {{ tab.label }}
        </button>
      </div>
      <label v-if="origin === 'menu'" class="ac-search"
        ><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i>
        <input
          ref="searchInput"
          v-model="query"
          :placeholder="placeholder"
          :aria-label="booksOnly ? '搜索平台作品' : '搜索资料或技能'"
          :aria-controls="listId"
          :aria-activedescendant="activeOptionId"
          @keydown="handleKeydown"
        />
      </label>
      <p v-else class="ac-mention-query" role="status">
        {{ query.trim() ? `搜索：${query.trim()}` : '继续在输入框输入名称或用途' }}
      </p>
      <el-select
        v-if="category === 'reference' && !booksOnly"
        v-model="referenceKind"
        class="ink-select ac-picker-kind"
        popper-class="ink-select-popper"
        aria-label="资料类型"
        @change="restoreTypingFocus"
      >
        <el-option label="全部资料" value="all" />
        <el-option v-for="(label, kind) in referenceLabels" :key="kind" :label="label" :value="kind" />
      </el-select>
      <div
        :id="listId"
        ref="results"
        class="ac-reference-results"
        role="listbox"
        :aria-label="booksOnly ? '平台作品选择结果' : '资料与技能选择结果'"
      >
        <template v-for="group in groups" :key="group.id">
          <div class="ac-picker-group" role="presentation">{{ group.label }}</div>
          <button
            v-for="option in group.options"
            :id="optionId(option)"
            :key="option.key"
            role="option"
            tabindex="-1"
            :aria-label="`${option.kind === 'skill' ? '技能' : '资料'}：${option.title}，${option.detail}`"
            :aria-selected="current?.key === option.key"
            :class="{ active: current?.key === option.key }"
            @mousedown.prevent
            @mouseenter="cursor = options.findIndex(item => item.key === option.key)"
            @click="select(option)"
          >
            <i :class="option.icon" aria-hidden="true"></i>
            <span
              ><strong>{{ option.title }}</strong
              ><small>{{ option.detail }}</small></span
            >
            <i v-if="isSelected(option)" class="fa-solid fa-check" aria-hidden="true"></i>
          </button>
        </template>
        <p v-if="!options.length" class="ac-small-empty">
          {{
            booksOnly
              ? '没有找到匹配的平台作品'
              : category === 'skill'
                ? '没有匹配的已启用技能'
                : '没有找到匹配的资料或技能'
          }}
        </p>
      </div>
      <div class="ac-picker-hint">
        <span v-if="limited" class="ac-picker-more">更多结果可继续搜索或切换分类</span>
        ↑↓ 选择 · Enter 确认 · Esc 关闭
      </div>
    </div>
  </el-popover>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useAgentWorkspace } from '../use-agent-workspace'
import { mentionAt, removeMention, type ComposerMention } from '../composer-mention'
import { referenceIcon } from '../reference-icons'
import { markdownToPlainText } from '@/utils/markdown'
import type { AgentReference, AgentReferenceKind, AgentSkill } from '@/types/agent-creation'

type Category = 'all' | 'reference' | 'skill'
type Option = { key: string; title: string; detail: string; icon: string } & (
  { kind: 'reference'; value: AgentReference } | { kind: 'skill'; value: AgentSkill }
)
const props = defineProps<{ input?: HTMLTextAreaElement }>()
const store = useAgentWorkspace()
const visible = ref(false)
const origin = ref<'mention' | 'menu'>('mention')
const booksOnly = ref(false)
const category = ref<Category>('all')
const referenceKind = ref<AgentReferenceKind | 'all'>('all')
const query = ref('')
const cursor = ref(0)
const panel = ref<HTMLElement>()
const results = ref<HTMLElement>()
const searchInput = ref<HTMLInputElement>()
const listId = `ac-picker-${crypto.randomUUID()}`
const composing = ref(false)
const placement = ref<'top-start' | 'bottom-start'>('top-start')
const availableHeight = ref(420)
const availableWidth = ref(380)
const EDGE = 12
const OFFSET = 12
// 面板传送到 body，以可见视口与工作区的交集为边界，不受输入区 overflow 影响。
const popperOptions = computed(() => ({
  modifiers: [
    { name: 'flip', enabled: false },
    {
      name: 'preventOverflow',
      options: {
        padding: EDGE,
        altAxis: true,
        tether: false,
        boundary: props.input?.closest<HTMLElement>('.ac-workspace') || 'clippingParents',
        rootBoundary: 'viewport'
      }
    }
  ]
}))
function updatePlacement() {
  if (!props.input) return
  const viewport = window.visualViewport
  const viewportTop = viewport?.offsetTop || 0
  const viewportBottom = viewportTop + (viewport?.height || window.innerHeight)
  const viewportLeft = viewport?.offsetLeft || 0
  const viewportRight = viewportLeft + (viewport?.width || window.innerWidth)
  const workspace = props.input.closest('.ac-workspace')?.getBoundingClientRect()
  const top = Math.max(viewportTop, workspace?.top ?? viewportTop)
  const bottom = Math.min(viewportBottom, workspace?.bottom ?? viewportBottom)
  const left = Math.max(viewportLeft, workspace?.left ?? viewportLeft)
  const right = Math.min(viewportRight, workspace?.right ?? viewportRight)
  const rect = props.input.getBoundingClientRect()
  const above = Math.max(0, rect.top - top - EDGE - OFFSET)
  const below = Math.max(0, bottom - rect.bottom - EDGE - OFFSET)
  placement.value = above >= below ? 'top-start' : 'bottom-start'
  availableHeight.value = Math.floor(Math.max(above, below))
  availableWidth.value = Math.floor(Math.max(0, Math.min(380, right - left - EDGE * 2)))
}
let layoutFrame = 0
let anchorObserver: ResizeObserver | undefined
function schedulePlacement() {
  if (!visible.value || layoutFrame) return
  layoutFrame = requestAnimationFrame(() => {
    layoutFrame = 0
    updatePlacement()
  })
}
watch(
  () => props.input,
  input => {
    anchorObserver?.disconnect()
    if (!input) return
    anchorObserver = new ResizeObserver(schedulePlacement)
    anchorObserver.observe(input)
    const workspace = input.closest('.ac-workspace')
    if (workspace) anchorObserver.observe(workspace)
    schedulePlacement()
  },
  { flush: 'post', immediate: true }
)
let mention: ComposerMention | null = null
let dismissed: string | null = null
const tabs: Array<{ id: Category; label: string }> = [
  { id: 'all', label: '全部' },
  { id: 'reference', label: '资料' },
  { id: 'skill', label: '技能' }
]
const referenceLabels: Record<AgentReferenceKind, string> = {
  book: '作品',
  volume: '卷',
  chapter: '章节',
  entity: '人物设定',
  breakdown: '拆书',
  rank: '榜单',
  material: '资料',
  selection: '选段'
}
const placeholder = computed(() =>
  booksOnly.value
    ? '搜索平台作品名称'
    : category.value === 'skill'
      ? '搜索技能名称或用途'
      : category.value === 'reference'
        ? '搜索资料名称或内容说明'
        : '搜索资料或技能'
)
const normalize = (value: string) => value.normalize('NFKC').toLocaleLowerCase().trim()
const matches = (title: string, detail: string) =>
  normalize(`${title} ${detail}`).includes(normalize(query.value))
const candidates = computed<Option[]>(() => {
  const skillOptions: Option[] =
    booksOnly.value || category.value === 'reference'
      ? []
      : store.skills
          .filter(skill => skill.enabled && matches(skill.name, `${skill.description} ${skill.id}`))
          .map(value => ({
            key: `skill:${value.id}`,
            kind: 'skill',
            value,
            title: value.name,
            detail: markdownToPlainText(value.description),
            icon: 'fa-solid fa-puzzle-piece'
          }))
  const references = new Map([...store.availableReferences, ...store.references].map(item => [item.id, item]))
  const referenceOptions: Option[] =
    category.value === 'skill'
      ? []
      : [...references.values()]
          .filter(
            value =>
              (!booksOnly.value || value.kind === 'book') &&
              (category.value !== 'reference' ||
                referenceKind.value === 'all' ||
                value.kind === referenceKind.value) &&
              matches(value.title, value.description)
          )
          .map(value => ({
            key: `reference:${value.id}`,
            kind: 'reference',
            value,
            title: value.title,
            detail: `${referenceLabels[value.kind]}${value.description ? ` · ${markdownToPlainText(value.description)}` : ''}`,
            icon: referenceIcon(value)
          }))
  const rank = (option: Option) =>
    normalize(option.title) === normalize(query.value)
      ? 0
      : normalize(option.title).startsWith(normalize(query.value))
        ? 1
        : 2
  skillOptions.sort((a, b) => rank(a) - rank(b))
  referenceOptions.sort((a, b) => rank(a) - rank(b))
  return [...skillOptions, ...referenceOptions]
})
// 保持资料和技能都可发现；大量章节通过搜索收窄，不一次渲染整本书的目录。
const options = computed(() =>
  category.value === 'all'
    ? [
        ...candidates.value.filter(item => item.kind === 'skill').slice(0, query.value.trim() ? 20 : 2),
        ...candidates.value.filter(item => item.kind === 'reference').slice(0, query.value.trim() ? 40 : 2)
      ]
    : candidates.value.slice(0, 60)
)
const limited = computed(() => candidates.value.length > options.value.length)
const groups = computed(() =>
  [
    {
      id: 'skill',
      label: '技能 · 指定本次使用的方法',
      options: options.value.filter(item => item.kind === 'skill')
    },
    {
      id: 'reference',
      label: booksOnly.value ? '平台作品' : '资料 · 提供参考内容',
      options: options.value.filter(item => item.kind === 'reference')
    }
  ].filter(group => group.options.length)
)
const current = computed(() => options.value[cursor.value])
const optionId = (option: Option) => `${listId}-${options.value.findIndex(item => item.key === option.key)}`
const activeOptionId = computed(() => (visible.value && current.value ? optionId(current.value) : undefined))
const isSelected = (option: Option) =>
  option.kind === 'skill'
    ? store.selectedSkillId === option.value.id
    : store.references.some(item => item.id === option.value.id)
watch([query, category, referenceKind], () => {
  cursor.value = 0
})
watch(options, items => {
  cursor.value = Math.max(0, Math.min(cursor.value, items.length - 1))
})
watch(cursor, () =>
  nextTick(() => {
    const row = results.value?.querySelector<HTMLElement>('[aria-selected="true"]')
    if (row && results.value) {
      const top = row.offsetTop - results.value.offsetTop
      if (top < results.value.scrollTop) results.value.scrollTop = top
      else if (top + row.offsetHeight > results.value.scrollTop + results.value.clientHeight)
        results.value.scrollTop = top + row.offsetHeight - results.value.clientHeight
    }
  })
)

function close() {
  if (mention) dismissed = `${mention.start}:${mention.trigger}`
  visible.value = false
  mention = null
}
function syncMention() {
  if (!props.input || composing.value) return
  const token = mentionAt(store.draft, props.input.selectionStart, props.input.selectionEnd)
  if (!token) {
    dismissed = null
    if (origin.value === 'mention') {
      visible.value = false
      mention = null
    }
    return
  }
  if (dismissed === `${token.start}:${token.trigger}`) return
  const changed =
    !mention ||
    mention.start !== token.start ||
    mention.trigger !== token.trigger ||
    origin.value !== 'mention'
  mention = token
  origin.value = 'mention'
  if (changed) {
    booksOnly.value = false
    category.value = token.trigger === '$' ? 'skill' : 'all'
    referenceKind.value = 'all'
  }
  query.value = token.query
  updatePlacement()
  visible.value = true
}
function open(next: Category, kind: AgentReferenceKind | 'all' = 'all') {
  booksOnly.value = false
  origin.value = 'menu'
  category.value = next
  referenceKind.value = kind
  query.value = ''
  cursor.value = 0
  mention = null
  updatePlacement()
  visible.value = true
  void nextTick(() => searchInput.value?.focus())
}
function openBooks() {
  open('reference', 'book')
  booksOnly.value = true
}
function restoreTypingFocus() {
  if (origin.value === 'mention') void nextTick(() => props.input?.focus())
}
function switchCategory(next: Category) {
  category.value = next
  referenceKind.value = 'all'
  restoreTypingFocus()
}
function select(option: Option) {
  if (option.kind === 'skill') store.selectedSkillId = option.value.id
  else if (origin.value === 'menu' && isSelected(option)) store.removeReference(option.value.id)
  else store.addReference(option.value)
  let caret = props.input?.selectionStart ?? store.draft.length
  if (origin.value === 'mention' && mention) {
    const currentMention = mentionAt(store.draft, caret, props.input?.selectionEnd)
    if (currentMention?.start === mention.start && currentMention.trigger === mention.trigger) {
      const edit = removeMention(store.draft, currentMention)
      store.draft = edit.text
      caret = edit.caret
    }
  }
  close()
  void nextTick(() => {
    props.input?.focus()
    props.input?.setSelectionRange(caret, caret)
  })
}
function handleKeydown(event: KeyboardEvent): boolean {
  if (!visible.value || event.isComposing || composing.value || event.keyCode === 229) return false
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    close()
    void nextTick(() => props.input?.focus())
    return true
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    event.stopPropagation()
    cursor.value = Math.max(
      0,
      Math.min(options.value.length - 1, cursor.value + (event.key === 'ArrowDown' ? 1 : -1))
    )
    return true
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    event.stopPropagation()
    if (current.value) select(current.value)
    return true
  }
  if (event.key === 'Tab') close()
  return false
}
function compositionStart() {
  composing.value = true
}
function compositionEnd() {
  composing.value = false
  void nextTick(syncMention)
}
function outside(event: PointerEvent) {
  const target = event.target as Node
  if (visible.value && origin.value === 'menu' && target === props.input) {
    close()
    return
  }
  if (
    visible.value &&
    target !== props.input &&
    !panel.value?.contains(target) &&
    !(target instanceof Element && target.closest('.ink-select-popper'))
  )
    close()
}
watch(
  () => store.activeId,
  () => {
    close()
    dismissed = null
  }
)
watch(
  () => store.draft,
  text => {
    if (!text) {
      close()
      dismissed = null
      return
    }
    if (dismissed) {
      const [start, trigger] = dismissed.split(':')
      if (text[Number(start)] !== trigger) dismissed = null
    }
    if (origin.value === 'mention' && mention && text[mention.start] !== mention.trigger) {
      close()
      dismissed = null
    }
  }
)
onMounted(() => {
  document.addEventListener('pointerdown', outside, true)
  window.addEventListener('resize', schedulePlacement)
  window.addEventListener('scroll', schedulePlacement, true)
  window.visualViewport?.addEventListener('resize', schedulePlacement)
  window.visualViewport?.addEventListener('scroll', schedulePlacement)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside, true)
  window.removeEventListener('resize', schedulePlacement)
  window.removeEventListener('scroll', schedulePlacement, true)
  window.visualViewport?.removeEventListener('resize', schedulePlacement)
  window.visualViewport?.removeEventListener('scroll', schedulePlacement)
  anchorObserver?.disconnect()
  cancelAnimationFrame(layoutFrame)
})
defineExpose({
  open,
  openBooks,
  close,
  syncMention,
  handleKeydown,
  compositionStart,
  compositionEnd,
  visible,
  listId,
  activeOptionId
})
</script>
