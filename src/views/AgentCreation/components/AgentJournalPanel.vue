<template>
  <section class="ac-canon ac-journal" aria-label="工作备注">
    <p v-if="loadError" class="ac-index-note warning" role="alert">{{ loadError }}</p>
    <div v-if="!days.length && !loading" class="ac-empty-state">
      <i class="fa-regular fa-calendar" aria-hidden="true"></i>
      <h3>还没有工作记录</h3>
    </div>
    <div v-else class="ac-journal-layout">
      <ul class="ac-journal-days" role="tablist" aria-label="日期">
        <li v-for="item in days" :key="item.date">
          <button role="tab" :aria-selected="item.date === date" :class="{ active: item.date === date }" @click="select(item.date)">
            <strong>{{ dayLabel(item.date) }}</strong>
            <small>{{ summaryLine(item) }}</small>
          </button>
        </li>
      </ul>
      <div v-if="day" class="ac-journal-day">
        <div v-if="day.notes.length" class="ac-canon-section">
          <header class="ac-canon-section-head"><h3>备注</h3></header>
          <ul class="ac-rule-list">
            <li v-for="note in day.notes" :key="note.id">
              <span class="ac-source-tag">{{ channelLabel(note.channel) }}</span>
              <div>
                <p>{{ note.text }}</p>
                <small>{{ timeOf(note.createdAt) }}<template v-if="note.sessionId"> · <a href="#" @click.prevent="openSession(note.sessionId)">查看对话</a></template></small>
              </div>
              <button class="ac-icon-button" :aria-label="`删除备注：${note.text}`" :disabled="busy" @click="removeNote(note)">
                <i class="fa-regular fa-trash-can" aria-hidden="true"></i>
              </button>
            </li>
          </ul>
        </div>
        <div v-if="day.writing.length" class="ac-canon-section">
          <header class="ac-canon-section-head"><h3>码字</h3></header>
          <ul class="ac-journal-list">
            <li v-for="item in day.writing" :key="item.bookId">
              <span>{{ item.title }}</span>
              <small>手写 {{ item.manual }} 字<template v-if="item.ai"> · AI {{ item.ai }} 字</template></small>
            </li>
          </ul>
        </div>
        <div v-if="day.chapters.length" class="ac-canon-section">
          <header class="ac-canon-section-head"><h3>章节改动</h3></header>
          <ul class="ac-journal-list">
            <li v-for="item in day.chapters" :key="item.chapterId">
              <span>{{ item.bookTitle }} · {{ item.title }}</span>
              <small>保存 {{ item.versions }} 次</small>
            </li>
          </ul>
        </div>
        <div v-if="day.sessions.length" class="ac-canon-section">
          <header class="ac-canon-section-head"><h3>对话</h3></header>
          <ul class="ac-journal-list">
            <li v-for="item in day.sessions" :key="item.sessionId">
              <a href="#" @click.prevent="openSession(item.sessionId)">{{ item.title }}</a>
              <small>{{ channelLabel(item.channel) }} · 作者 {{ item.userMessages }} 条 · 回复 {{ item.replies }} 条</small>
            </li>
          </ul>
        </div>
        <div v-if="day.artifacts.length" class="ac-canon-section">
          <header class="ac-canon-section-head"><h3>成果</h3></header>
          <ul class="ac-journal-list">
            <li v-for="item in day.artifacts" :key="item.id">
              <span>{{ item.title }}</span>
              <small>{{ kindLabel(item.kind) }} · {{ timeOf(item.createdAt) }}</small>
            </li>
          </ul>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentJournalDay, AgentJournalDaySummary, AgentJournalNote } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'

const store = useAgentWorkspace()
const router = useRouter()
const days = ref<AgentJournalDaySummary[]>([])
const day = ref<AgentJournalDay | null>(null)
const date = ref('')
const loading = ref(false)
const busy = ref(false)
const loadError = ref('')

const CHANNEL: Record<string, string> = { desktop: '桌面', weixin: '微信' }
const KIND: Record<string, string> = { chapter: '章节候选', short: '短篇', report: '报告', plan: '草案', skill: '技能' }
const channelLabel = (value?: string) => CHANNEL[value || ''] || value || '桌面'
const kindLabel = (value: string) => KIND[value] || value
const timeOf = (ms: number) => new Date(ms).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
const dayLabel = (value: string) => {
  const today = new Date()
  const stamp = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
  if (value === stamp(today)) return '今天'
  today.setDate(today.getDate() - 1)
  if (value === stamp(today)) return '昨天'
  return value
}
const summaryLine = (item: AgentJournalDaySummary) =>
  [
    item.words ? `${item.words} 字` : '',
    item.chapters ? `${item.chapters} 章` : '',
    item.sessions ? `${item.sessions} 次对话` : '',
    item.notes ? `${item.notes} 条备注` : ''
  ]
    .filter(Boolean)
    .join(' · ')

async function load() {
  loading.value = true
  loadError.value = ''
  try {
    days.value = await store.journalDays()
    if (!days.value.some(item => item.date === date.value)) date.value = days.value[0]?.date || ''
    day.value = date.value ? await store.journalDay(date.value) : null
  } catch (error) {
    loadError.value = String(error)
  } finally {
    loading.value = false
  }
}
async function select(value: string) {
  date.value = value
  try {
    day.value = await store.journalDay(value)
  } catch (error) {
    loadError.value = String(error)
  }
}
async function removeNote(note: AgentJournalNote) {
  try {
    await inkConfirm('删除这条备注？', '删除备注', { confirmButtonText: '删除', type: 'warning' })
  } catch {
    return
  }
  busy.value = true
  try {
    await store.deleteJournalNote(note.id)
    await load()
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
async function openSession(id: string) {
  if (!store.conversations.some(item => item.id === id)) {
    ElMessage.warning('这次对话已不在列表里')
    return
  }
  store.selectConversation(id)
  await router.push('/agent-creation')
}
onMounted(load)
</script>

<style scoped>
.ac-journal-layout {
  display: grid;
  grid-template-columns: 180px minmax(0, 1fr);
  gap: 20px;
}
.ac-journal-days {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.ac-journal-days button {
  width: 100%;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 8px 10px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: transparent;
  text-align: left;
  cursor: pointer;
}
.ac-journal-days button.active {
  border-color: var(--el-border-color);
  background: var(--el-fill-color-light);
}
.ac-journal-days small {
  color: var(--el-text-color-secondary);
}
.ac-journal-list {
  list-style: none;
  margin: 0;
  padding: 0;
}
.ac-journal-list li {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  padding: 6px 0;
  border-bottom: 1px dashed var(--el-border-color-lighter);
}
.ac-journal-list small {
  color: var(--el-text-color-secondary);
  white-space: nowrap;
}
@media (max-width: 720px) {
  .ac-journal-layout {
    grid-template-columns: 1fr;
  }
  .ac-journal-days {
    flex-direction: row;
    flex-wrap: wrap;
  }
}
</style>
