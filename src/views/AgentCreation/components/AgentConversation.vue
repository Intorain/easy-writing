<template>
  <div ref="scrollArea" class="ac-messages-scroll">
    <div class="ac-messages" aria-live="polite" aria-relevant="additions text">
      <article v-for="group in messageGroups" :key="group.id" class="ac-message" :class="`is-${group.role}`">
        <template v-if="group.role === 'user'">
          <div v-if="group.messages[0].source === 'breakdown'" class="ac-message-source">
            <i class="fa-solid fa-book-open" aria-hidden="true"></i>拆书完成，自动接着分析
          </div>
          <div class="ac-user-message">{{ group.messages[0].text }}</div>
          <div
            v-if="group.messages[0].references?.length || group.messages[0].selectedSkill"
            class="ac-message-references"
          >
            <span v-if="group.messages[0].selectedSkill"
              ><i class="fa-solid fa-puzzle-piece" aria-hidden="true"></i>技能：{{
                group.messages[0].selectedSkill.name
              }}</span
            >
            <span v-for="reference in group.messages[0].references" :key="reference.id"
              ><i :class="referenceIcon(reference)" aria-hidden="true"></i>{{ reference.title }}</span
            >
          </div>
          <div v-if="group.messages[0].text.trim()" class="ac-message-actions is-user">
            <time
              v-if="group.sent"
              class="ac-sent-time"
              :datetime="group.sent.iso"
              :title="`发送于 ${group.sent.sentAt}`"
              >{{ group.sent.sentAt }}</time
            >
            <button title="复制内容" aria-label="复制消息" @click="copyText(group.messages[0].text)">
              <i class="fa-regular fa-copy" aria-hidden="true"></i>
            </button>
          </div>
        </template>
        <template v-else>
          <div class="ac-assistant-identity"><img src="/logo.png" alt="" /><strong>易创</strong></div>
          <div class="ac-assistant-body">
            <div class="ac-reply-divider">
              <span v-if="group.timing">{{ group.timing.duration }}</span>
              <span
                v-if="group.usage"
                class="ac-reply-usage"
                :title="`输入 ${group.usage.input_tokens} · 输出 ${group.usage.output_tokens} tokens`"
                >{{ formatTokens(group.usage.total_tokens) }}</span
              >
              <span class="ac-reply-rule" aria-hidden="true"></span>
            </div>
            <div v-for="item in group.messages" :key="item.id" class="ac-assistant-segment">
              <div v-if="item.kind === 'activity' && item.activity" class="ac-activity">
                <details class="ac-inline-details">
                  <summary>
                    <i
                      :class="{
                        'fa-solid fa-circle-notch fa-spin': item.activity.status === 'running',
                        'fa-solid fa-circle-check ac-status-done': item.activity.status === 'completed',
                        'fa-regular fa-circle-pause': item.activity.status === 'paused',
                        'fa-regular fa-circle-stop': item.activity.status === 'stopped'
                      }"
                      aria-hidden="true"
                    ></i>
                    <span>{{ activityLabel(item.activity) }}</span>
                    <i class="fa-solid fa-chevron-right ac-details-chevron" aria-hidden="true"></i>
                  </summary>
                  <div class="ac-activity-steps">
                    <div
                      v-for="(step, index) in item.activity.steps"
                      :key="step"
                      :class="{ done: index < item.activity.step }"
                    >
                      <i
                        :class="
                          index < item.activity.step
                            ? 'fa-solid fa-check'
                            : index === item.activity.step && item.activity.status === 'running'
                              ? 'fa-solid fa-circle-notch fa-spin'
                              : 'fa-regular fa-circle'
                        "
                        aria-hidden="true"
                      ></i
                      ><span>{{ step }}</span>
                    </div>
                  </div>
                </details>
                <button
                  v-if="item.activity.status === 'paused'"
                  class="ac-button ink-btn ink-btn-outline ink-btn-sm"
                  @click="store.resume()"
                >
                  继续任务
                </button>
              </div>
              <template v-else-if="item.kind === 'choice'">
                <template v-if="item.choice?.status === 'pending'">
                  <div v-if="item.prompt?.kind === 'steps'" class="ac-message-text">
                    可以逐项回答，也可以直接在下方输入框说明你的想法或新的要求。
                  </div>
                  <!-- eslint-disable-next-line vue/no-v-html -- 共用解析器禁用原始 HTML 并校验链接协议。 -->
                  <div v-else class="ac-message-text" v-html="renderMarkdown(item.text)"></div>
                  <AgentPromptCard :message="item" />
                </template>
                <details v-else class="ac-inline-details ac-choice-history">
                  <summary>
                    <i :class="item.choice?.status === 'superseded' ? 'fa-solid fa-arrow-turn-up' : 'fa-solid fa-check'" aria-hidden="true"></i>
                    <span>{{ answeredLabel(item) }}</span>
                    <i class="fa-solid fa-chevron-right ac-details-chevron" aria-hidden="true"></i>
                  </summary>
                  <ul aria-label="作者的选择" class="ac-choice-history-list">
                    <li
                      v-for="choice in item.choice?.options || []"
                      :key="choice.id"
                      :class="{ selected: isPicked(item, choice.id) }"
                    >
                      <i
                        :class="
                          isPicked(item, choice.id) ? 'fa-solid fa-circle-check' : 'fa-regular fa-circle'
                        "
                        aria-hidden="true"
                      ></i>
                      <span
                        ><strong>{{ choice.title }}</strong
                        ><small>{{ choice.description }}</small></span
                      >
                    </li>
                    <li v-for="line in answerLines(item)" :key="line" class="selected">
                      <i class="fa-regular fa-pen-to-square" aria-hidden="true"></i><span>{{ line }}</span>
                    </li>
                  </ul>
                </details>
              </template>
              <template v-else>
                <!-- eslint-disable-next-line vue/no-v-html -- 共用解析器禁用原始 HTML 并校验链接协议。 -->
                <div v-if="item.text.trim()" class="ac-message-text" v-html="renderMarkdown(item.text)"></div>
                <ol v-if="item.bullets?.length" class="ac-findings">
                  <li v-for="point in item.bullets" :key="point.title">
                    <strong>{{ point.title }}</strong>
                    <!-- eslint-disable-next-line vue/no-v-html -- 共用解析器禁用原始 HTML 并校验链接协议。 -->
                    <div class="ac-message-text" v-html="renderMarkdown(point.text)"></div>
                  </li>
                </ol>
                <template v-if="item.kind === 'result'">
                  <AgentDistillationPanel v-if="item.distillationId && distillationJob(item.distillationId)" :job="distillationJob(item.distillationId)!" />
                  <button v-else-if="item.breakdown" class="ac-artifact-link" @click="store.openBreakdown(item.breakdown)">
                    <i class="fa-solid fa-book-open" aria-hidden="true"></i>
                    <span><strong>《{{ item.breakdown.title }}》拆书结果</strong><small>第 {{ item.breakdown.from }}–{{ item.breakdown.to }} 章 · {{ breakdownStatus(item) }} · 保存在本次对话</small></span>
                    <span class="ac-artifact-action">查看<i class="fa-solid fa-arrow-right" aria-hidden="true"></i></span>
                  </button>
                  <AgentProseMessage v-else-if="item.artifactId && proseArtifact(item.artifactId)" :artifact-id="item.artifactId" />
                  <button v-else class="ac-artifact-link" @click="openArtifact(item.artifactId)">
                    <i class="fa-regular fa-file-lines" aria-hidden="true"></i>
                    <span
                      ><strong>{{ artifactTitle(item.artifactId) }}</strong
                      ><small>{{ artifactDescription(item.artifactId) }}</small></span
                    >
                    <span class="ac-artifact-action"
                      >查看<i class="fa-solid fa-arrow-right" aria-hidden="true"></i
                    ></span>
                  </button>
                  <AgentBreakdownPanel v-if="breakdownJob(item) && ['running','paused','failed'].includes(breakdownJob(item)!.status)" :job-value="breakdownJob(item)!" @continue="$emit('continue')" />
                  <div
                    v-if="item.bullets?.length && store.active?.status === 'completed'"
                    class="ac-result-actions"
                  >
                    <button class="ac-button ink-btn ink-btn-primary" @click="store.previewRewrite()">
                      <i class="fa-solid fa-pen" aria-hidden="true"></i>生成修改预览
                    </button>
                    <button class="ac-button ink-btn ink-btn-outline" @click="$emit('continue')">
                      <i class="fa-regular fa-comment" aria-hidden="true"></i>继续讨论
                    </button>
                  </div>
                  <span v-if="item.bullets?.length" class="ac-result-note">
                    <i class="fa-solid fa-circle-info" aria-hidden="true"></i>本次未修改正文
                  </span>
                </template>
              </template>
            </div>
            <div v-if="group.canCopy" class="ac-message-actions">
              <button title="复制内容" aria-label="复制回复" @click="copy(group.messages)">
                <i class="fa-regular fa-copy" aria-hidden="true"></i>
              </button>
              <time
                v-if="group.timing"
                class="ac-reply-time"
                :datetime="group.timing.iso"
                :title="`回答于 ${group.timing.answeredAt}`"
                >{{ group.timing.answeredAt }}</time
              >
            </div>
          </div>
        </template>
      </article>
      <p v-if="store.active?.status === 'running'" role="status">
        <i class="fa-solid fa-circle-notch fa-spin" aria-hidden="true"></i>
        {{ store.active.currentTool ? toolLabel(store.active.currentTool) : '正在想…' }}
        <span v-if="store.active.currentTool === 'get_rank_collection' && store.active.currentToolProgress">
          （{{ store.active.currentToolProgress.completed }}/{{ store.active.currentToolProgress.total }}）
        </span>
      </p>
      <p v-if="store.active?.error" role="alert">{{ store.active.error }}</p>
      <p v-if="store.active?.status !== 'running' && store.active?.taskOutcome?.message" class="ac-muted" role="status">
        {{ store.active.taskOutcome.message }}
      </p>
      <p v-if="store.active?.interactionWarning" class="ac-muted" role="status">
        {{ store.active.interactionWarning }}
      </p>
      <section
        v-if="store.active?.memoryWarning || memoryProblems.length"
        class="ac-memory-status"
        role="status"
      >
        <p class="ac-muted">{{ store.active?.memoryWarning || '记忆尚有待处理的来源，稿件已经保留。' }}</p>
        <details v-if="memoryProblems.length" class="ac-inline-details">
          <summary>查看记忆处理结果</summary>
          <div v-for="entry in memoryProblems" :key="entry.phase">
            <p>
              {{ entry.phase === 'author' ? '作者决定' : '作品资料' }}：已处理 {{ entry.value.completed }} /
              {{ entry.value.total }} 份来源，尝试 {{ entry.value.attempts }} 次。
            </p>
            <p v-if="entry.value.deferred">另有 {{ entry.value.deferred }} 份来源待下一批处理。</p>
            <p v-for="(failure, index) in entry.value.failures" :key="index">{{ failure.message }}</p>
          </div>
        </details>
        <button
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          type="button"
          :disabled="store.submitting || store.active?.status !== 'completed'"
          @click="store.retryMemory()"
        >
          重试整理记忆
        </button>
      </section>
      <button
        v-if="store.active && ['paused', 'failed'].includes(store.active.status)"
        class="ac-button ink-btn ink-btn-outline"
        @click="store.resume()"
      >
        继续任务
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { referenceIcon } from '../reference-icons'
import { ElMessage } from 'element-plus'
import AgentBreakdownPanel from './AgentBreakdownPanel.vue'
import AgentDistillationPanel from './AgentDistillationPanel.vue'
import type { AgentMessage } from '@/types/agent-creation'
// 共用解析器禁用原始 HTML；仅在展示时转换，存储和复制继续使用 Markdown 原文。
import { renderMarkdown } from '@/utils/markdown'
import { useAgentWorkspace } from '../use-agent-workspace'
import { groupMessages, replyTiming, sentTiming } from '../message-presentation'
import AgentPromptCard from './AgentPromptCard.vue'
import AgentProseMessage from './AgentProseMessage.vue'

defineEmits<{ continue: [] }>()
const store = useAgentWorkspace()
const memoryProblems = computed(() =>
  Object.entries(store.active?.memoryStatus || {})
    .filter(([, value]) => ['partial', 'failed'].includes(value.state) || (value.deferred || 0) > 0)
    .map(([phase, value]) => ({ phase, value }))
)
/** 等待时只说在做哪类事，不把工具步骤念给作者听 */
const toolLabel = (name: string) => {
  if (['draft_chapter', 'draft_short_story'].includes(name)) return '正在写稿…'
  if (['polish_draft', 'revise_spans', 'check_chapter'].includes(name)) return '正在核对与修改…'
  if (['save_chapter', 'apply_creation_artifact', 'save_material', 'create_chapter', 'create_volume'].includes(name)) return '正在保存…'
  if (['plan_scenes', 'plan_chapters', 'plan_short_story', 'resolve_ruleset', 'assemble_context', 'prepare_book_plan'].includes(name))
    return '正在构思…'
  if (name.startsWith('get_rank') || name.startsWith('list_rank') || name === 'rank_trends') return '正在查榜单…'
  if (name.startsWith('read_') || name.startsWith('list_') || name.startsWith('search_') || ['recall_memory', 'load_book_context', 'open_book'].includes(name))
    return '正在读资料…'
  if (name === 'ask_author') return '等你回复'
  return '正在处理…'
}
/** 本轮用量：千以上按 k 显示 */
const formatTokens = (value: number) =>
  value >= 1000 ? `${(value / 1000).toFixed(value >= 100000 ? 0 : 1)}k tokens` : `${value} tokens`
const scrollArea = ref<HTMLElement>()
const messageGroups = computed(() =>
  groupMessages((store.active?.messages || []).filter(item => !item.choice?.supersededBy)).map(group => ({
    ...group,
    timing: replyTiming(group.messages),
    usage: group.messages.find(item => item.usage?.total_tokens)?.usage || null,
    sent: group.role === 'user' ? sentTiming(group.messages[0]) : null,
    canCopy:
      !(store.active?.status === 'running' && group.runId === store.active.runId) &&
      copyableMessages(group.messages).length > 0
  }))
)
/** 已回答的引导卡：选中项（多选全部）、表单值与作者补充的话 */
const pickedIds = (item: AgentMessage) =>
  item.choice?.selectedIds || (item.choice?.selectedId ? [item.choice.selectedId] : [])
const isPicked = (item: AgentMessage, id: string) => pickedIds(item).includes(id)
const answerLines = (item: AgentMessage) => {
  const fields = item.prompt?.fields || []
  const values = item.answer?.fields || {}
  const blank = item.answer?.blank || []
  const stepLines = (item.answer?.answers || []).map(
    step =>
      `${step.question.replace(/[？?：:]$/, '')}：${[step.selected.join('、'), step.text || ''].filter(Boolean).join('；')}`
  )
  return [
    ...stepLines,
    ...fields.filter(field => values[field.key]).map(field => `${field.label}：${values[field.key]}`),
    ...(blank.length ? [`交给 Agent 建议：${blank.join('、')}`] : []),
    ...(item.answer?.text ? [item.answer.text] : [])
  ]
}
const answeredLabel = (item: AgentMessage) => {
  if (item.choice?.status === 'superseded') return '已通过新消息继续，此问题不再等待回答'
  if (item.choice?.status !== 'selected') return '已通过对话继续'
  const titles = pickedIds(item)
    .map(id => item.choice?.options?.find(choice => choice.id === id)?.title)
    .filter(Boolean)
  if (titles.length) return `已选择：${titles.join('、')}`
  if (item.prompt?.kind === 'steps')
    return `已回答 ${item.answer?.answers?.length || 0}/${item.prompt.steps?.length || 0} 问`
  return item.prompt?.kind === 'form' ? '已填写' : '已回答'
}
const activityLabel = (activity: NonNullable<AgentMessage['activity']>) => {
  if (activity.status === 'completed') return '资料整理完成'
  if (activity.status === 'paused') return '任务已暂停'
  if (activity.status === 'stopped') return '任务已结束'
  return `正在${activity.steps[activity.step] || '整理资料'}`
}
const proseText = (item: AgentMessage) =>
  item.kind === 'result' && item.artifactId && proseArtifact(item.artifactId)
    ? store.artifacts.find(a => a.id === item.artifactId)?.content.trim() || ''
    : ''
const copyableMessages = (items: AgentMessage[]) =>
  items.filter(
    item =>
      item.kind !== 'activity' &&
      item.kind !== 'choice' &&
      (item.text.trim() || proseText(item) || item.bullets?.some(point => point.title.trim() || point.text.trim()))
  )
const distillationJob = (id: string) => store.distillationJobs.find(job => job.id === id)
const breakdownJob = (message: AgentMessage) => message.breakdown ? store.breakdownJobs.find(job => job.sessionId === store.activeId && job.projectId === message.breakdown?.projectId && job.createdAt === message.breakdown?.createdAt) : undefined
const breakdownStatus = (message: AgentMessage) => ({ running: '拆解中', paused: '已暂停', failed: '待重试', completed: '已结束', cancelled: '已取消' })[breakdownJob(message)?.status || message.breakdown!.status]
const artifactTitle = (id?: string) => store.artifacts.find(item => item.id === id)?.title || '查看内容'
const proseArtifact = (id: string) => {
  const a = store.artifacts.find(item => item.id === id)
  return !!a && ['chapter', 'short', 'draft'].includes(a.kind)
}
const artifactDescription = (id?: string) => {
  const a = store.artifacts.find(item => item.id === id)
  if (a?.kind === 'book_plan') return a.applied ? '作品已创建' : '开书方案 · 可以查看和修改'
  if (a?.kind === 'skill') return a.applied ? '已保存的技能' : '技能候选 · 可继续对话修改'
  if (a?.kind === 'selection') return `选题卡 ${a.selectionCards?.length || 0} 张 · 比较后选定方向`
  if (a?.applied) return '已保存到作品'
  return '已保留 · 可以继续完善'
}
function openArtifact(id?: string) {
  if (id) store.openedArtifactId = id
  else store.openReport()
}
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    ElMessage.success('已复制')
  } catch (error) {
    console.warn('复制 Agent 内容失败', error)
    ElMessage.warning('复制失败，请选择内容手动复制')
  }
}
function copy(items: AgentMessage[]) {
  return copyText(
    copyableMessages(items)
      .map(item =>
        [item.text, proseText(item), ...(item.bullets?.map(point => `${point.title}\n${point.text}`) || [])]
          .filter(Boolean)
          .join('\n\n')
      )
      .join('\n\n')
  )
}
watch(
  () => [store.activeId, store.active?.messages.length, store.active?.status, store.openedArtifactId],
  async () => {
    await nextTick()
    if (scrollArea.value) scrollArea.value.scrollTop = scrollArea.value.scrollHeight
  },
  { immediate: true }
)
</script>
