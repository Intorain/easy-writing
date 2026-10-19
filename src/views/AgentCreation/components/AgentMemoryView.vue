<template>
  <section class="ac-manager ac-memory-manager" aria-label="记忆管理">
    <div class="ac-manager-heading">
      <div class="ac-tabs ac-heading-tabs" role="tablist" aria-label="记忆与作品状态">
        <button
          v-for="item in tabs"
          :key="item.id"
          role="tab"
          :aria-selected="tab === item.id"
          :class="{ active: tab === item.id }"
          @click="tab = item.id"
        >
          {{ item.label }}
        </button>
      </div>
      <div v-if="isMemoryTab" class="ac-actions">
        <button class="ac-button ink-btn ink-btn-outline" :disabled="refreshing" @click="refresh">
          刷新来源
        </button>
        <button class="ac-button ink-btn ink-btn-outline" @click="edit()">
          <i class="fa-solid fa-plus" aria-hidden="true"></i>添加记忆
        </button>
      </div>
    </div>
    <div class="ac-memory-book">
      <div v-if="tab !== 'author' && tab !== 'journal'" class="ac-select-wrap">
        <el-select
          v-model="bookId"
          class="ink-select"
          popper-class="ink-select-popper"
          aria-label="选择记忆所属作品"
        >
          <template #prefix><i class="fa-solid fa-book-open" aria-hidden="true"></i></template>
          <el-option
            v-for="book in store.availableReferences.filter(r => r.kind === 'book')"
            :key="book.id"
            :value="book.bookId"
            :label="book.title"
          />
        </el-select>
      </div>
    </div>
    <AgentJournalPanel v-if="tab === 'journal'" />
    <template v-else-if="!isMemoryTab">
      <div v-if="!bookId" class="ac-empty-state">
        <i class="fa-solid fa-book-open" aria-hidden="true"></i>
        <h3>还没有可查看的作品</h3>
        <p>在书架建立作品后，这里会显示它的状态账本、规则与文风。</p>
      </div>
      <AgentCanonPanel v-else-if="tab === 'canon'" :book-id="bookId" />
      <AgentRulesPanel v-else-if="tab === 'rules'" :book-id="bookId" />
      <AgentStylePanel v-else :book-id="bookId" />
    </template>
    <template v-else>
      <div class="ac-manager-filters">
        <label class="ac-search"
          ><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i
          ><input v-model="query" aria-label="搜索记忆" placeholder="搜索记忆内容..."
        /></label>
        <div class="ac-filter-buttons" role="group" aria-label="记忆状态">
          <button
            v-for="item in statuses"
            :key="item.id"
            :aria-pressed="status === item.id"
            :class="{ active: status === item.id }"
            @click="status = item.id"
          >
            {{ item.label }}
          </button>
        </div>
      </div>
      <div class="ac-manager-content" :class="{ 'has-detail': selected }">
        <div class="ac-manager-list">
          <button
            v-for="memory in filtered"
            :key="memory.id"
            class="ac-memory-row"
            :class="{ selected: selectedId === memory.id }"
            @click="selectedId = memory.id"
          >
            <i class="fa-regular fa-file-lines" aria-hidden="true"></i
            ><span
              ><strong>{{ memory.content }}</strong
              ><small>{{ memory.category }} · 来源：{{ memory.source }}</small></span
            ><i class="fa-solid fa-chevron-right ac-tiny" aria-hidden="true"></i>
          </button>
          <div v-if="!filtered.length" class="ac-empty-state">
            <i class="fa-regular fa-bookmark" aria-hidden="true"></i>
            <h3>{{ query ? '没有找到相关记忆' : '这里还没有记忆' }}</h3>
            <p>{{ query ? '试试其他关键词，或调整筛选条件。' : '记录作品设定，让下一次创作有据可依。' }}</p>
            <button v-if="query" class="ac-button ink-btn ink-btn-outline" @click="query = ''">清除搜索</button
            ><button v-else class="ac-button ink-btn ink-btn-outline" @click="edit()">添加第一条记忆</button>
          </div>
        </div>
        <aside v-if="selected" class="ac-detail" aria-label="记忆详情">
          <div class="ac-detail-title">
            <h3>记忆详情</h3>
            <button class="ac-icon-button" aria-label="关闭记忆详情" @click="selectedId = ''">
              <i class="fa-solid fa-xmark" aria-hidden="true"></i>
            </button>
          </div>
          <h4>{{ selected.content }}</h4>
          <p v-if="selected.status === 'stale'" class="ac-muted">
            {{ selected.staleReason || '来源已改变，这条记忆暂不用于创作。' }}
          </p>
          <p v-if="selected.status === 'rejected'" class="ac-muted">此项作为否决约束使用，不作为作品事实。</p>
          <p v-if="selected.partial" class="ac-muted">这条摘要只覆盖来源的部分内容。</p>
          <p v-if="selected.digest && selected.status === 'pending'" class="ac-muted">
            写作上下文的固定层超出上限，写作会一直被拒绝。确认后删去下面列出的重复条目（保留的那条原话不变）；仍超限就需要你再删减或缩短规则。
          </p>
          <p v-if="selected.suggestedRule && selected.status === 'pending'" class="ac-muted">
            确认后会建立写作规则「{{ selected.suggestedRule.text }}」，之后的起草与体检都会遵守；不对就点“否决”，不会再提。
          </p>
          <dl>
            <dt>生效范围</dt>
            <dd>{{ selected.scope === 'author' ? '所有作品' : '当前作品' }}</dd>
            <dt>状态</dt>
            <dd>
              <span class="ac-status-dot" :class="selected.status"></span>{{ statusLabel(selected.status) }}
            </dd>
            <dt>来源</dt>
            <dd>
              <button class="ac-text-link" @click="sourceVisible = true">{{ selected.source }}</button>
            </dd>
            <dt v-if="selected.kind">记忆类型</dt>
            <dd v-if="selected.kind">
              {{ { decision: '作者决定', summary: '来源摘要', candidate: '待定方案' }[selected.kind] }}
            </dd>
            <dt>适用阶段</dt>
            <dd>{{ selected.period || '未限定' }}</dd>
            <dt v-if="selected.observedAt">记录时间</dt>
            <dd v-if="selected.observedAt">{{ new Date(selected.observedAt).toLocaleString('zh-CN') }}</dd>
            <dt v-if="selected.channel">来自入口</dt>
            <dd v-if="selected.channel">{{ { desktop: '桌面', weixin: '微信' }[selected.channel] || selected.channel }}</dd>
            <dt v-if="selected.replacedBy">已被取代</dt>
            <dd v-if="selected.replacedBy">由后来的一条记忆替代，不再生效</dd>
          </dl>
          <div class="ac-detail-section">
            <span class="ac-muted">来源摘录 / 作者备注</span>
            <blockquote>{{ selected.quote || '这条记忆由作者直接添加。' }}</blockquote>
          </div>
          <div class="ac-detail-actions">
            <button
              v-if="selected.status === 'pending'"
              class="ac-button ink-btn ink-btn-primary"
              @click="confirmMemory"
            >
              {{ selected.digest ? '确认并删去重复条目' : selected.suggestedRule ? '确认并设为规则' : '确认记忆' }}</button
            ><button v-else class="ac-button ink-btn ink-btn-primary" @click="edit(selected)">
              <i class="fa-solid fa-pen" aria-hidden="true"></i>编辑</button
            ><button
              v-if="selected.status === 'pending'"
              class="ac-button ink-btn ink-btn-outline"
              @click="edit(selected)"
            >
              编辑</button
            ><button class="ac-button ink-btn ink-btn-outline" @click="remove">
              <i class="fa-regular fa-trash-can" aria-hidden="true"></i>删除
            </button>
            <button
              v-if="['active', 'pending'].includes(selected.status)"
              class="ac-button ink-btn ink-btn-outline"
              @click="rejectMemory"
            >
              否决
            </button>
            <button v-if="selected.sourceRef" class="ac-button ink-btn ink-btn-outline" @click="openSource">
              打开来源
            </button>
          </div>
        </aside>
      </div>
    </template>
    <EwModal
      v-model:visible="formVisible"
      :title="form.id ? '编辑记忆' : '添加记忆'"
      width="540px"
      custom-class="ac-dialog"
      :close-on-click-modal="false"
      manage-focus
    >
      <form id="agent-memory-form" class="ac-form" @submit.prevent="save">
        <label
          >记忆内容<textarea
            class="ink-input"
            v-model="form.content"
            required
            :maxlength="form.contentLimit || 1200"
            rows="3"
            placeholder="记录一个明确的事实或创作要求"
          ></textarea>
        </label>
        <div class="ac-form-grid">
          <label for="agent-memory-scope"
            >生效范围
            <el-select
              id="agent-memory-scope"
              v-model="form.scope"
              class="ink-select"
              popper-class="ink-select-popper"
              aria-label="生效范围"
            >
              <el-option value="book" label="当前作品" />
              <el-option value="author" label="作者长期偏好" />
            </el-select>
          </label>
          <label for="agent-memory-status"
            >状态
            <el-select
              id="agent-memory-status"
              v-model="form.status"
              class="ink-select"
              popper-class="ink-select-popper"
              aria-label="记忆状态"
            >
              <el-option value="active" label="已采用 / 有效" />
              <el-option value="pending" label="待定" />
              <el-option value="rejected" label="已否决" />
              <el-option value="stale" label="待更新（请先核对内容）" disabled />
              <el-option value="archived" label="已归档" />
            </el-select>
          </label>
        </div>
        <label
          >分类<input
            class="ink-input"
            v-model="form.category"
            required
            maxlength="30"
            placeholder="如：人物特征、写作规则" /></label
        ><label
          >适用阶段<input
            class="ink-input"
            v-model="form.period"
            maxlength="60"
            placeholder="如：全书、身份揭晓之前" /></label
        ><label
          >来源或备注<textarea
            class="ink-input"
            v-model="form.quote"
            rows="2"
            maxlength="600"
            placeholder="补充这条记忆的依据"
          ></textarea>
        </label>
      </form>
      <template #footer>
        <button type="button" class="ac-button ink-btn ink-btn-outline" @click="formVisible = false">
          取消</button
        ><button class="ac-button ink-btn ink-btn-primary" type="submit" form="agent-memory-form">
          保存记忆
        </button>
      </template>
    </EwModal>
    <EwModal
      v-model:visible="sourceVisible"
      title="记忆来源"
      width="480px"
      custom-class="ac-dialog"
      manage-focus
      ><template v-if="selected"
        ><p class="ac-muted">{{ selected.source }}</p>
        <blockquote class="ac-source-quote">{{ selected.quote || selected.content }}</blockquote>
        <p class="ac-muted">{{ selected?.source || '作者记录' }}</p></template
      ></EwModal
    >
  </section>
</template>

<script setup lang="ts">
import EwModal from '@/components/EwModal/index.vue'
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentMemory } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
import AgentCanonPanel from './AgentCanonPanel.vue'
import AgentRulesPanel from './AgentRulesPanel.vue'
import AgentStylePanel from './AgentStylePanel.vue'
import AgentJournalPanel from './AgentJournalPanel.vue'

type MemoryTab = 'book' | 'author' | 'journal' | 'canon' | 'rules' | 'style'
const tabs: Array<{ id: MemoryTab; label: string }> = [
  { id: 'book', label: '作品记忆' },
  { id: 'author', label: '作者偏好' },
  { id: 'journal', label: '工作备注' },
  { id: 'canon', label: '作品状态' },
  { id: 'rules', label: '规则与画像' },
  { id: 'style', label: '文风' }
]
const store = useAgentWorkspace()
const route = useRoute()
const router = useRouter()
const refreshing = ref(false)
// 写作台“本章体检”等入口可带 ?tab=canon&bookId=… 直接打开对应页签
const initialTab = tabs.find(item => item.id === route.query.tab)?.id
const tab = ref<MemoryTab>(initialTab || 'book')
const isMemoryTab = computed(() => tab.value === 'book' || tab.value === 'author')
const scope = computed<AgentMemory['scope']>(() => (tab.value === 'author' ? 'author' : 'book'))
const bookId = ref(typeof route.query.bookId === 'string' ? route.query.bookId : '')
watch(
  () => [route.query.tab, route.query.bookId],
  ([nextTab, nextBook]) => {
    const found = tabs.find(item => item.id === nextTab)?.id
    if (found) tab.value = found
    if (typeof nextBook === 'string' && nextBook) bookId.value = nextBook
  }
)
const query = ref('')
const status = ref<AgentMemory['status']>('active')
const selectedId = ref('')
const formVisible = ref(false)
const sourceVisible = ref(false)
const statuses: Array<{ id: AgentMemory['status']; label: string }> = [
  { id: 'active', label: '有效' },
  { id: 'pending', label: '待定' },
  { id: 'rejected', label: '已否决' },
  { id: 'stale', label: '待更新' },
  { id: 'archived', label: '已归档' }
]
const inScope = computed(() =>
  store.memories.filter(
    item => item.scope === scope.value && (scope.value === 'author' || item.bookId === bookId.value)
  )
)
const filtered = computed(() =>
  inScope.value.filter(
    item =>
      item.status === status.value &&
      `${item.content}${item.category}${item.source}`.includes(query.value.trim())
  )
)
const selected = computed(() => filtered.value.find(item => item.id === selectedId.value))
const statusLabel = (value: AgentMemory['status']) =>
  ({ active: '已采用 / 有效', pending: '待定', rejected: '已否决', stale: '来源已变化', archived: '已归档' })[
    value
  ]
watch(
  () => store.availableReferences,
  () => {
    if (!bookId.value)
      bookId.value =
        store.active?.contextBookId ||
        store.active?.targetBookId ||
        store.availableReferences.find(r => r.kind === 'book')?.bookId ||
        ''
  },
  { immediate: true }
)
const blank = (): AgentMemory => ({
  id: '',
  bookId: bookId.value,
  content: '',
  category: '写作规则',
  scope: scope.value,
  status: 'active',
  source: '作者添加',
  quote: '',
  period: '全书'
})
const form = ref<AgentMemory>(blank())
async function refresh() {
  if (refreshing.value) return
  refreshing.value = true
  try {
    await store.initialize()
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    refreshing.value = false
  }
}
onMounted(refresh)
async function rejectMemory() {
  if (!selected.value) return
  try {
    await store.saveMemory({ ...selected.value, status: 'rejected' })
    status.value = 'rejected'
    ElMessage.success('已记录为否决约束')
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function openSource() {
  const source = selected.value?.sourceRef
  if (!source) return
  if (source.type === 'author') {
    if (!store.conversations.some(s => s.id === source.sessionId)) {
      sourceVisible.value = true
      return
    }
    store.selectConversation(source.sessionId!)
    await router.push('/agent-creation')
  } else {
    await router.push({
      name: 'Writing',
      params: { bookId: source.bookId },
      query: source.chapterId ? { from: 'agent', chapterId: source.chapterId } : {}
    })
  }
}
watch([scope, bookId, status, query], () => {
  selectedId.value = filtered.value[0]?.id || ''
})
function edit(memory?: AgentMemory) {
  form.value = memory ? { ...memory } : blank()
  formVisible.value = true
}
async function save() {
  try {
    if (!form.value.content.trim() || !form.value.category.trim()) return
    const saved = await store.saveMemory({
      ...form.value,
      content: form.value.content.trim(),
      bookId: form.value.scope === 'author' ? '' : bookId.value
    })
    formVisible.value = false
    tab.value = form.value.scope
    status.value = form.value.status
    selectedId.value = saved.id
    ElMessage.success('记忆已保存')
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function confirmMemory() {
  try {
    const memory = selected.value
    if (memory?.digest) {
      const result = await store.applyDigest(memory.bookId)
      status.value = 'archived'
      ElMessage.success(`已删去 ${result.removed} 条重复的规则与否决约束`)
      return
    }
    if (memory?.suggestedRule)
      await store.saveRule({
        scope: 'author',
        bookId: '',
        category: memory.suggestedRule.category,
        text: memory.suggestedRule.text
      })
    if (memory) await store.saveMemory({ ...memory, status: 'active' })
    status.value = 'active'
    ElMessage.success(memory?.suggestedRule ? '已设为写作规则' : '已确认这条记忆')
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function remove() {
  if (!selected.value) return
  const id = selected.value.id
  try {
    await inkConfirm('删除这条记忆？其他记忆与作品内容不受影响。', '删除记忆', {
      confirmButtonText: '删除',
      cancelButtonText: '取消',
      type: 'warning'
    })
    await store.deleteMemory(id)
    selectedId.value = filtered.value[0]?.id || ''
    ElMessage.success('记忆已删除')
  } catch (error) {
    if (error !== 'cancel' && error !== 'close') ElMessage.error(String(error))
  }
}
</script>
