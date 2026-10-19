<template>
  <section class="ac-manager ac-automation-manager" aria-label="自动任务">
    <div class="ac-manager-heading">
      <div>
        <h2>自动任务</h2>
        <p>到点替你做事，结果发到微信或在桌面提醒</p>
      </div>
      <div class="ac-actions">
        <button class="ac-button ink-btn ink-btn-outline" :disabled="busy" @click="load">刷新</button>
      </div>
    </div>
    <p v-if="loadError" class="ac-index-note warning" role="alert">{{ loadError }}</p>
    <div class="ac-canon">
      <section class="ac-canon-section">
        <header class="ac-canon-section-head"><div><h3>任务</h3></div></header>
        <div v-if="!tasks.length" class="ac-empty-state">
          <i class="fa-regular fa-clock" aria-hidden="true"></i>
          <h3>还没有自动任务</h3>
        </div>
        <div v-else class="ac-automation-cards">
          <article v-for="task in tasks" :key="task.id" class="ac-index-card" :class="{ 'is-off': !task.enabled }">
            <div class="ac-index-head">
              <div class="ac-index-title">
                <strong>
                  {{ task.title }}
                  <em class="ac-canon-tag">{{ scheduleLabel(task.schedule) }}</em>
                </strong>
                <small>{{ statusLine(task) }}</small>
              </div>
              <el-switch :model-value="task.enabled" :disabled="busy" aria-label="启用" @change="(value: string | number | boolean) => toggle(task, Boolean(value))" />
            </div>
            <p v-if="task.instruction" class="ac-automation-instruction">{{ task.instruction }}</p>
            <p v-if="task.lastSummary" class="ac-automation-summary">{{ task.lastSummary }}</p>
            <div class="ac-automation-controls">
              <el-select
                :model-value="task.deliver"
                class="ink-select ac-automation-select"
                popper-class="ink-select-popper"
                size="small"
                :disabled="busy"
                aria-label="结果送到"
                @change="(value: string) => setDeliver(task, value)"
              >
                <el-option v-for="option in DELIVER_OPTIONS" :key="option.value" :label="option.label" :value="option.value" />
              </el-select>
              <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy || !task.enabled" @click="runNow(task)">立即运行</button>
              <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy" @click="toggleRuns(task)">
                {{ expanded === task.id ? '收起记录' : '运行记录' }}
              </button>
              <button v-if="!task.builtin" class="ac-icon-button" :aria-label="`删除任务：${task.title}`" :disabled="busy" @click="remove(task)">
                <i class="fa-regular fa-trash-can" aria-hidden="true"></i>
              </button>
            </div>
            <ul v-if="expanded === task.id" class="ac-rule-list ac-automation-runs">
              <li v-if="!runs.length"><div><small>还没有运行记录</small></div></li>
              <li v-for="run in runs" :key="run.runId">
                <span class="ac-source-tag">{{ statusLabel(run.status) }}</span>
                <div>
                  <small>
                    {{ timeOf(run.startedAt) }} · {{ durationOf(run) }}<template v-if="run.merged"> · 合并补跑 {{ run.merged }} 次</template>
                    <template v-if="run.sessionId"> · <a href="#" @click.prevent="openSession(run.sessionId)">查看会话</a></template>
                  </small>
                  <p v-if="run.summary">{{ run.summary }}</p>
                  <p v-if="run.error" class="ac-automation-error">{{ run.error }}</p>
                </div>
              </li>
            </ul>
          </article>
        </div>
      </section>

      <section class="ac-canon-section">
        <header class="ac-canon-section-head">
          <div>
            <h3>事件提醒</h3>
            <p class="ac-automation-hours">
              只在
              <el-input-number v-model="hoursStart" :min="0" :max="23" :step="1" size="small" aria-label="开始小时" @change="saveHours" />
              点到
              <el-input-number v-model="hoursEnd" :min="1" :max="24" :step="1" size="small" aria-label="结束小时" @change="saveHours" />
              点之间提醒
            </p>
          </div>
          <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy" @click="checkNow">现在检查一次</button>
        </header>
        <ul class="ac-rule-list ac-automation-rules">
          <li v-for="rule in rules" :key="rule.event" :class="{ 'is-off': !rule.enabled }">
            <div class="ac-automation-rule-label">
              <template v-for="(part, index) in labelParts(rule)" :key="index">
                <el-input-number
                  v-if="part.param && rule.paramRange"
                  :model-value="rule.params?.[part.param] ?? rule.paramRange.default"
                  :min="rule.paramRange.min"
                  :max="rule.paramRange.max"
                  :step="1"
                  size="small"

                  :disabled="busy"
                  :aria-label="part.param"
                  @change="(value: number | undefined) => setParam(rule, part.param!, value)"
                />
                <strong v-else>{{ part.text }}</strong>
              </template>
            </div>
            <el-select
              v-if="rule.event !== 'idle_consolidate'"
              :model-value="rule.deliver"
              class="ink-select ac-automation-select"
              popper-class="ink-select-popper"
              size="small"
              :disabled="busy"
              aria-label="送到"
              @change="(value: string) => setRule(rule, { deliver: value })"
            >
              <el-option v-for="option in DELIVER_OPTIONS" :key="option.value" :label="option.label" :value="option.value" />
            </el-select>
            <el-switch :model-value="rule.enabled" :disabled="busy" aria-label="启用" @change="(value: string | number | boolean) => setRule(rule, { enabled: Boolean(value) })" />
          </li>
        </ul>
      </section>

      <section class="ac-canon-section">
        <header class="ac-canon-section-head">
          <div>
            <h3>通知</h3>
            <p>任务结果、巡检提醒和事件提醒都先记在这里，再发到微信或桌面；没发出去的会补发</p>
          </div>
          <button v-if="pendingCount" class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy" @click="flush">重试发送（{{ pendingCount }}）</button>
        </header>
        <div v-if="!deliveries.length" class="ac-empty-state">
          <i class="fa-regular fa-paper-plane" aria-hidden="true"></i>
          <h3>还没有通知</h3>
        </div>
        <ul v-else class="ac-rule-list ac-automation-runs">
          <li v-for="item in deliveries" :key="item.id">
            <span class="ac-source-tag">{{ channelLabel(item.channel) }} · {{ item.status === 'sent' ? '已发出' : '待发' }}</span>
            <div>
              <p>{{ item.text }}</p>
              <small>{{ timeOf(item.createdAt) }}<template v-if="item.attempts"> · 尝试 {{ item.attempts }} 次</template><template v-if="item.lastError"> · {{ item.lastError }}</template></small>
            </div>
          </li>
        </ul>
      </section>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentAutomationRule, AgentAutomationRun, AgentAutomationSchedule, AgentAutomationTask, AgentDeliveryRecord } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'

const store = useAgentWorkspace()
const router = useRouter()
const tasks = ref<AgentAutomationTask[]>([])
const rules = ref<AgentAutomationRule[]>([])
const deliveries = ref<AgentDeliveryRecord[]>([])
const runs = ref<AgentAutomationRun[]>([])
const expanded = ref('')
const busy = ref(false)
const loadError = ref('')

const DELIVER_OPTIONS = [
  { value: 'weixin', label: '发到微信' },
  { value: 'desktop', label: '桌面提醒' },
  { value: 'none', label: '不通知' },
]
const STATUS: Record<string, string> = { ok: '完成', silent: '无事', waiting: '等你回答', failed: '失败', timeout: '超时' }
const hoursStart = ref(8)
const hoursEnd = ref(23)
/** 把"伏笔超过 {chapters} 章没推进时提醒"拆成文字与数字格 */
const labelParts = (rule: AgentAutomationRule) =>
  rule.label.split(/(\{\w+\})/).filter(Boolean).map(piece => {
    const match = piece.match(/^\{(\w+)\}$/)
    return match ? { param: match[1] } : { text: piece }
  }) as Array<{ param?: string; text?: string }>
const CHANNEL: Record<string, string> = { weixin: '微信', desktop: '桌面' }
const WEEKDAYS = ['一', '二', '三', '四', '五', '六', '日']
const statusLabel = (value?: string) => STATUS[value || ''] || value || ''
const channelLabel = (value: string) => CHANNEL[value] || value
const thousands = (value: number) => (value >= 1000 ? `${(value / 1000).toFixed(1)} 千` : String(value))
const pendingCount = computed(() => deliveries.value.filter(item => item.status === 'pending').length)
const timeOf = (ms?: number | null) =>
  ms ? new Date(ms).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : ''
const durationOf = (run: AgentAutomationRun) => {
  const seconds = Math.max(0, Math.round((run.finishedAt - run.startedAt) / 1000))
  return seconds >= 60 ? `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒` : `${seconds} 秒`
}
const scheduleLabel = (schedule: AgentAutomationSchedule) => {
  switch (schedule.type) {
    case 'daily':
      return `每天 ${schedule.time}`
    case 'weekly':
      return `每周${WEEKDAYS[Math.min(7, Math.max(1, schedule.weekday || 1)) - 1]} ${schedule.time}`
    case 'interval': {
      const minutes = schedule.minutes || 0
      return minutes % 60 === 0 ? `每 ${minutes / 60} 小时` : `每 ${minutes} 分钟`
    }
    case 'once':
      return `${timeOf(schedule.at)} 一次`
    default:
      return ''
  }
}
/** 卡片第二行：下次、上次结果、错误、用量，有什么说什么 */
const statusLine = (task: AgentAutomationTask) => {
  const parts: string[] = []
  if (task.enabled) parts.push(`下次 ${timeOf(task.nextRunAt)}`)
  else parts.push(task.disabledReason ? `已停用：${task.disabledReason}` : '已停用')
  if (task.lastRunAt) parts.push(`上次 ${timeOf(task.lastRunAt)} ${statusLabel(task.lastStatus)}`)
  if (task.lastError) parts.push(task.lastError)
  if (task.estimate && task.kind === 'agent') parts.push(`约 ${thousands(task.estimate.weeklyTokens)} token/周`)
  return parts.join(' · ')
}

async function load() {
  loadError.value = ''
  try {
    const [taskList, ruleList, deliveryList, settings] = await Promise.all([
      store.automationList(),
      store.automationRules(),
      store.deliveryRecent(),
      store.automationSettings(),
    ])
    tasks.value = taskList
    rules.value = ruleList
    deliveries.value = deliveryList
    hoursStart.value = settings.activeHours?.[0] ?? 8
    hoursEnd.value = settings.activeHours?.[1] ?? 23
    if (expanded.value) runs.value = await store.automationRuns(expanded.value)
  } catch (error) {
    loadError.value = String(error)
  }
}
async function act(work: () => Promise<unknown>) {
  busy.value = true
  try {
    await work()
    await load()
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
const toggle = (task: AgentAutomationTask, enabled: boolean) => act(() => store.automationUpdate(task.id, { enabled }))
const setDeliver = (task: AgentAutomationTask, deliver: string) => act(() => store.automationUpdate(task.id, { deliver }))
const setRule = (rule: AgentAutomationRule, patch: { enabled?: boolean; deliver?: string }) => act(() => store.automationRuleUpdate(rule.event, patch))
const runNow = (task: AgentAutomationTask) => act(() => store.automationRunNow(task.id))
const setParam = (rule: AgentAutomationRule, name: string, value: number | undefined) => {
  if (!Number.isFinite(value)) return
  return act(() => store.automationRuleUpdate(rule.event, { params: { [name]: Math.round(Number(value)) } }))
}
const saveHours = () => act(() => store.automationSettingsUpdate({ activeHours: [hoursStart.value, hoursEnd.value] }))
const checkNow = () =>
  act(async () => {
    const result = await store.automationCheckNow()
    ElMessage.success(result.alerts ? `查到 ${result.alerts} 件事，已送出提醒` : '查过了，没有要提醒的')
  })
const flush = () => act(() => store.deliveryFlush())
async function toggleRuns(task: AgentAutomationTask) {
  if (expanded.value === task.id) {
    expanded.value = ''
    return
  }
  expanded.value = task.id
  try {
    runs.value = await store.automationRuns(task.id)
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function remove(task: AgentAutomationTask) {
  try {
    await inkConfirm(`删除自动任务「${task.title}」？运行记录一并删除。`, '删除任务', { confirmButtonText: '删除', type: 'warning' })
  } catch {
    return
  }
  await act(() => store.automationDelete(task.id))
}
async function openSession(id: string) {
  if (!store.conversations.some(item => item.id === id)) {
    ElMessage.warning('这次会话已不在列表里')
    return
  }
  store.selectConversation(id)
  await router.push('/agent-creation')
}
watch(() => store.automationVersion, () => void load())
onMounted(load)
</script>

<style scoped>
.ac-automation-cards {
  display: grid;
  gap: 12px;
}
.ac-index-card.is-off .ac-index-title,
.ac-index-card.is-off .ac-automation-instruction,
.ac-index-card.is-off .ac-automation-summary {
  opacity: 0.55;
}
.ac-index-title strong {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.ac-index-title strong .ac-canon-tag {
  margin-left: 0;
}
.ac-automation-instruction,
.ac-automation-summary {
  margin: 0;
  white-space: pre-wrap;
  font-size: 0.875rem;
  line-height: 1.7;
}
.ac-automation-summary {
  padding: 8px 12px;
  border-left: 2px solid var(--ac-line);
  color: var(--ac-muted);
}
.ac-automation-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
.ac-automation-select {
  width: 128px;
}
.ac-automation-runs > li {
  align-items: flex-start;
}
.ac-automation-runs p {
  margin: 4px 0 0;
  white-space: pre-wrap;
  font-size: 0.875rem;
  line-height: 1.7;
}
.ac-automation-runs small,
.ac-automation-rules small {
  color: var(--ac-muted);
}
.ac-automation-rules > li {
  align-items: center;
}
.ac-automation-rule-label {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.ac-automation-rule-label :deep(.el-input-number) {
  width: 88px;
}
.ac-automation-hours {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.ac-automation-hours :deep(.el-input-number) {
  width: 88px;
}
.ac-automation-rules > li.is-off > div {
  opacity: 0.55;
}
.ac-automation-error {
  color: var(--state-danger-on, #8b2f26);
}
</style>
