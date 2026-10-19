<template>
  <div class="ac-steps">
    <p class="ac-step-note">{{ submissionHint }}</p>
    <div class="ac-steps-progress">
      <span>第 {{ index + 1 }} / {{ steps.length }} 问</span>
      <ol aria-label="问题进度">
        <li v-for="(step, i) in steps" :key="step.key">
          <button
            type="button"
            :class="{ current: i === index, done: answered(step), skipped: i < index && !answered(step) }"
            :aria-label="`第 ${i + 1} 问：${step.question}${answered(step) ? '（已答）' : ''}`"
            :aria-current="i === index ? 'step' : undefined"
            @click="go(i)"
          ></button>
        </li>
      </ol>
    </div>

    <section :key="current.key" class="ac-step" :aria-label="current.question">
      <h4>
        {{ current.question }}<em v-if="current.required" aria-hidden="true">*</em
        ><span v-if="current.required" class="ac-sr-only">（必答）</span>
      </h4>
      <p v-if="current.hint" class="ac-step-hint">{{ current.hint }}</p>
      <fieldset v-if="current.options?.length">
        <legend class="ac-sr-only">{{ current.multiple ? '可以选多项' : '选择一项' }}</legend>
        <label
          v-for="option in current.options"
          :key="option.id"
          class="ac-choice"
          :class="{ selected: currentAnswer.selectedIds.includes(option.id) }"
        >
          <input
            :type="current.multiple ? 'checkbox' : 'radio'"
            :name="`${messageId}:${current.key}`"
            :value="option.id"
            :checked="currentAnswer.selectedIds.includes(option.id)"
            @click="toggle(option.id)"
          /><span
            ><strong>{{ option.title }}</strong
            ><small v-if="option.description">{{ option.description }}</small>
            <details v-if="option.preview" class="ac-prompt-preview" @click.stop>
              <summary>看示例</summary>
              <p>{{ option.preview }}</p>
            </details></span
          >
        </label>
      </fieldset>
      <label class="ac-prompt-free">
        <span>{{ current.options?.length ? '或者自己写（也可以补充所选项）' : '你的回答' }}</span>
        <textarea
          ref="input"
          class="ink-input"
          rows="2"
          :value="currentAnswer.text"
          :placeholder="current.placeholder || '都不合适就写你的想法'"
          @input="setText(($event.target as HTMLTextAreaElement).value)"
          @keydown="onKeydown"
        ></textarea>
      </label>
    </section>

    <div class="ac-choice-footer">
      <button v-if="index > 0" class="ac-text-link" type="button" @click="go(index - 1)">
        <i class="fa-solid fa-arrow-left" aria-hidden="true"></i>上一问
      </button>
      <span v-else class="ac-step-note">{{ notice }}</span>
      <button
        class="ac-button ink-btn ink-btn-sm"
        :class="answered(current) ? 'ink-btn-primary' : 'ink-btn-outline'"
        type="button"
        :disabled="submitting || (current.required && !answered(current))"
        @click="advance"
      >
        {{ actionLabel }}
      </button>
    </div>
    <p v-if="index > 0 && notice" class="ac-step-note">{{ notice }}</p>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import type { AgentPromptDraft } from '@/types/agent-creation'
import type { AuthorAnswer, PromptStep } from '@/types/canon'

/**
 * 分步问答：问题、提示与建议选项全部来自 Agent 的引导卡（AuthorPrompt.steps），这里只负责一次显示一问、
 * 记住每问的回答（存在会话的 promptDraft 里，切走再回来不丢），最后一次性提交。
 * 跳过的选填问题不算漏答，宿主会把它们列给 Agent，由 Agent 先给建议。
 */
const props = defineProps<{ messageId: string; steps: PromptStep[]; draft: AgentPromptDraft; submitting: boolean }>()
const emit = defineEmits<{ update: [draft: AgentPromptDraft]; submit: [answer: AuthorAnswer] }>()
const input = ref<HTMLTextAreaElement>()
const notice = ref('')
const submissionHint = computed(() =>
  props.steps.some(step => step.required)
    ? '提交这张卡时，带 * 的问题需要回答；其余可以跳过。'
    : '这些问题都可以跳过，交给 Agent 先给建议。'
)

type StepValue = { selectedIds: string[]; text: string }
const answers = computed<Record<string, StepValue>>(() => props.draft.steps || {})
const index = computed(() => Math.min(Math.max(props.draft.step ?? 0, 0), props.steps.length - 1))
const current = computed(() => props.steps[index.value])
const currentAnswer = computed<StepValue>(() => answers.value[current.value.key] || { selectedIds: [], text: '' })
const last = computed(() => index.value === props.steps.length - 1)
const answered = (step: PromptStep) => {
  const value = answers.value[step.key]
  return !!value && (value.selectedIds.length > 0 || !!value.text.trim())
}
const actionLabel = computed(() => {
  if (answered(current.value)) return last.value ? '完成并提交' : '下一问'
  if (current.value.required) return '下一问'
  return last.value ? '跳过并提交' : '跳过，交给 Agent 建议'
})

function save(next: Partial<AgentPromptDraft>) {
  emit('update', { ...props.draft, ...next })
}
function setAnswer(value: StepValue) {
  save({ steps: { ...answers.value, [current.value.key]: value } })
}
function toggle(id: string) {
  const selected = currentAnswer.value.selectedIds
  const picked = selected.includes(id)
  setAnswer({
    ...currentAnswer.value,
    selectedIds: current.value.multiple
      ? picked
        ? selected.filter(item => item !== id)
        : [...selected, id]
      : picked
        ? []
        : [id]
  })
}
function setText(text: string) {
  setAnswer({ ...currentAnswer.value, text })
}
function go(target: number) {
  notice.value = ''
  save({ step: target })
  void nextTick(() => input.value?.focus({ preventScroll: true }))
}
function advance() {
  if (!last.value) {
    go(index.value + 1)
    return
  }
  // 跳着答时可能漏了必答题：带作者回到那一问，而不是提交后被宿主退回
  const missing = props.steps.findIndex(step => step.required && !answered(step))
  if (missing >= 0) {
    go(missing)
    notice.value = '这一问必须回答，答完再提交。'
    return
  }
  const steps = Object.fromEntries(
    props.steps
      .filter(answered)
      .map(step => {
        const value = answers.value[step.key]
        return [step.key, value.text.trim() ? { selectedIds: value.selectedIds, text: value.text.trim() } : { selectedIds: value.selectedIds }]
      })
  )
  emit('submit', { selectedIds: [], steps })
}
/** 回车进入下一问，Shift+回车换行；输入法选字时的回车不算 */
function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || event.shiftKey || event.isComposing || event.keyCode === 229) return
  event.preventDefault()
  if (!(current.value.required && !answered(current.value))) advance()
}
</script>
