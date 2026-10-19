<template>
  <section class="ac-canon" aria-label="作品状态">
    <div class="ac-index-card">
      <div class="ac-index-head">
        <div class="ac-index-title">
          <strong>已索引 {{ progress?.indexed ?? 0 }} / {{ progress?.total ?? 0 }} 章</strong>
          <small>{{ indexSummary }}</small>
        </div>
        <div class="ac-actions">
          <button
            class="ac-button ink-btn ink-btn-outline"
            :disabled="busy || !!progress?.paused || !missingCount"
            :title="missingCount ? `为 ${missingCount} 章建立章节卡` : '全部章节都已索引'"
            @click="buildMissing"
          >
            <i class="fa-solid fa-rotate" aria-hidden="true"></i>补建索引
          </button>
          <el-dropdown trigger="click" popper-class="ac-conversation-menu" @command="onIndexCommand">
            <button class="ac-icon-button" aria-label="更多索引操作" :disabled="busy">
              <i class="fa-solid fa-ellipsis" aria-hidden="true"></i>
            </button>
            <template #dropdown>
              <el-dropdown-menu>
                <el-dropdown-item command="now" :disabled="!!progress?.paused"
                  >立即处理排队的变更</el-dropdown-item
                >
                <el-dropdown-item command="rebuild" :disabled="!!progress?.paused"
                  >全书重新提取</el-dropdown-item
                >
                <el-dropdown-item command="pause" divided>{{
                  progress?.paused ? '恢复本书索引' : '暂停本书索引'
                }}</el-dropdown-item>
              </el-dropdown-menu>
            </template>
          </el-dropdown>
        </div>
      </div>
      <div
        class="ac-index-bar"
        role="progressbar"
        aria-label="索引进度"
        :aria-valuenow="percent"
        aria-valuemin="0"
        aria-valuemax="100"
      >
        <span :style="{ width: `${percent}%` }"></span>
      </div>
      <p v-if="store.indexNotice" class="ac-index-note warning">{{ store.indexNotice }}</p>
      <p v-if="failures.length" class="ac-index-note warning">
        {{ failures.length }} 章提取失败：{{
          failures
            .slice(0, 3)
            .map(item => `${chapterTitle(item.chapterId)}（${item.message}）`)
            .join('；')
        }}{{
          failures.length > 3 ? ' 等' : ''
        }}。连续失败三次的章节不再自动重试，可用“全书重新提取”或修改章节后重试。
      </p>
      <p v-if="pendingEntities.length" class="ac-index-note">
        正文里有 {{ pendingEntities.length }} 个称呼没对上人物表：{{ pendingEntities.slice(0, 8).join('、')
        }}{{
          pendingEntities.length > 8 ? ' 等' : ''
        }}。在角色资料里新增人物或补上别名，重新提取相关章节后即可解析。
      </p>
    </div>

    <div class="ac-segmented" role="tablist" aria-label="作品状态分类">
      <button
        v-for="item in views"
        :key="item.id"
        role="tab"
        :aria-selected="view === item.id"
        :class="{ active: view === item.id }"
        @click="view = item.id"
      >
        {{ item.label }}<span v-if="item.count" class="ac-tab-count">{{ item.count }}</span>
      </button>
    </div>

    <p v-if="loadError" class="ac-index-note warning" role="alert">{{ loadError }}</p>

    <!-- 人物状态 -->
    <div v-if="view === 'states'" class="ac-manager-content" :class="{ 'has-detail': selectedState }">
      <div class="ac-manager-list">
        <button
          v-for="item in states"
          :key="item.characterId"
          class="ac-memory-row"
          :class="{ selected: selectedStateId === item.characterId }"
          @click="selectState(item.characterId)"
        >
          <i class="fa-regular fa-user" aria-hidden="true"></i
          ><span
            ><strong
              >{{ item.name || '未命名人物'
              }}<em v-if="!item.latest?.alive" class="ac-canon-tag danger">已死亡</em></strong
            ><small>{{ stateLine(item.latest) }}</small
            ><small
              >最近出场：{{ chapterTitle(item.lastAppearedChapter) }} · 状态变化 {{ item.changes }} 次</small
            ></span
          ><i class="fa-solid fa-chevron-right ac-tiny" aria-hidden="true"></i>
        </button>
        <div v-if="!states.length" class="ac-empty-state">
          <i class="fa-regular fa-user" aria-hidden="true"></i>
          <h3>还没有人物状态</h3>
          <p>建立索引后，人物的境界、所在地、持有物、生死与知情范围会按故事顺序汇总在这里。</p>
        </div>
      </div>
      <aside v-if="selectedState" class="ac-detail" aria-label="人物状态详情">
        <div class="ac-detail-title">
          <h3>{{ selectedState.name || '未命名人物' }}</h3>
          <button class="ac-icon-button" aria-label="关闭人物状态" @click="selectedStateId = ''">
            <i class="fa-solid fa-xmark" aria-hidden="true"></i>
          </button>
        </div>
        <label class="ac-canon-field"
          >查看某一章时的状态
          <el-select
            v-model="asOfChapter"
            class="ink-select"
            popper-class="ink-select-popper"
            aria-label="选择章节"
            placeholder="最新状态"
            filterable
          >
            <el-option label="最新状态" value="" />
            <el-option v-for="item in cardChapters" :key="item.id" :label="item.title" :value="item.id" />
          </el-select>
        </label>
        <p v-if="asOfLoading" class="ac-muted">正在读取…</p>
        <p v-else-if="asOfChapter && asOf && !asOf.known" class="ac-muted">
          这一章之前还没有该人物的状态记录。
        </p>
        <dl v-else>
          <dt>生死</dt>
          <dd>{{ shownState?.alive === false ? '已死亡' : '在世' }}</dd>
          <dt>所在地</dt>
          <dd>{{ shownState?.location || '未提及' }}</dd>
          <dt>境界</dt>
          <dd>{{ shownState?.realm || '未提及' }}</dd>
          <dt>持有物</dt>
          <dd>{{ shownState?.items?.length ? shownState.items.join('、') : '未提及' }}</dd>
          <dt>关系</dt>
          <dd>{{ relationText || '未提及' }}</dd>
          <dt>目标</dt>
          <dd>{{ shownState?.goals?.length ? shownState.goals.join('；') : '未提及' }}</dd>
        </dl>
        <div v-if="asOfChapter && asOf?.knows?.length" class="ac-detail-section">
          <span class="ac-muted">到这一章为止知道的事</span>
          <ul class="ac-canon-knows">
            <li v-for="item in asOf.knows" :key="`${item.factId}:${item.chapterId}`">
              {{ factText(item.factId) }}
              <small
                >{{ chapterTitle(item.chapterId) }}{{ item.viaNarration ? ' · 只有读者知道' : '' }}</small
              >
            </li>
          </ul>
        </div>
        <p class="ac-muted ac-canon-footnote">
          状态由索引器从正文提取，是检索线索而不是定论；前后说法矛盾时会列入“疑点”由你裁决。
        </p>
      </aside>
    </div>

    <!-- 线索账本 -->
    <template v-else-if="view === 'threads'">
      <div class="ac-filter-buttons ac-canon-filters" role="group" aria-label="线索状态">
        <button
          v-for="item in threadFilters"
          :key="item.id"
          :aria-pressed="threadFilter === item.id"
          :class="{ active: threadFilter === item.id }"
          @click="threadFilter = item.id"
        >
          {{ item.label }}
        </button>
      </div>
      <div class="ac-manager-content" :class="{ 'has-detail': selectedThread }">
        <div class="ac-manager-list">
          <button
            v-for="item in filteredThreads"
            :key="item.threadId"
            class="ac-memory-row"
            :class="{ selected: selectedThreadId === item.threadId }"
            @click="selectThread(item)"
          >
            <i class="fa-solid fa-link" aria-hidden="true"></i
            ><span
              ><strong>{{ item.title }}</strong
              ><small
                >{{ THREAD_KIND_LABELS[item.kind] || item.kind }} · 埋于{{
                  chapterTitle(item.plantedChapter)
                }}
                · 推进 {{ item.advances.length }} 次 · {{ THREAD_STATUS_LABELS[item.status]
                }}{{ item.overridden ? '（作者标注）' : '' }}</small
              ></span
            ><i class="fa-solid fa-chevron-right ac-tiny" aria-hidden="true"></i>
          </button>
          <div v-if="!filteredThreads.length" class="ac-empty-state">
            <i class="fa-solid fa-link" aria-hidden="true"></i>
            <h3>没有这类线索</h3>
            <p>索引器会记录每章新埋的伏笔、悬念、承诺与感情线，以及它们在哪一章推进和回收。</p>
          </div>
        </div>
        <aside v-if="selectedThread" class="ac-detail" aria-label="线索详情">
          <div class="ac-detail-title">
            <h3>{{ THREAD_KIND_LABELS[selectedThread.kind] || '线索' }}</h3>
            <button class="ac-icon-button" aria-label="关闭线索详情" @click="selectedThreadId = ''">
              <i class="fa-solid fa-xmark" aria-hidden="true"></i>
            </button>
          </div>
          <h4>{{ selectedThread.title }}</h4>
          <div class="ac-detail-section">
            <span class="ac-muted">埋于{{ chapterTitle(selectedThread.plantedChapter) }}</span>
            <blockquote>{{ selectedThread.plantedQuote }}</blockquote>
          </div>
          <div v-if="selectedThread.advances.length" class="ac-detail-section">
            <span class="ac-muted">推进与回收</span>
            <ul class="ac-canon-knows">
              <li v-for="(item, index) in selectedThread.advances" :key="index">
                {{ item.quote }}
                <small
                  >{{ item.action === 'payoff' ? '回收' : '推进' }} ·
                  {{ chapterTitle(item.chapterId) }}</small
                >
              </li>
            </ul>
          </div>
          <form class="ac-form ac-detail-section" @submit.prevent="saveThread">
            <label for="agent-thread-status"
              >线索状态
              <el-select
                id="agent-thread-status"
                v-model="threadForm.status"
                class="ink-select"
                popper-class="ink-select-popper"
                aria-label="线索状态"
              >
                <el-option value="open" label="未回收" />
                <el-option value="paid" label="已回收" />
                <el-option value="dropped" label="已放弃（不再提醒）" />
              </el-select>
            </label>
            <label
              >备注<input
                v-model="threadForm.note"
                class="ink-input"
                maxlength="200"
                placeholder="如：留到第三卷回收"
            /></label>
            <p class="ac-muted ac-form-note">你标注的状态会覆盖索引器的判断，重新索引也不会改回。</p>
            <button class="ac-button ink-btn ink-btn-primary" type="submit" :disabled="busy">保存标注</button>
          </form>
        </aside>
      </div>
    </template>

    <!-- 疑点 -->
    <template v-else-if="view === 'suspects'">
      <ul v-if="suspects.length" class="ac-quality-list ac-canon-suspects">
        <li v-for="item in suspects" :key="item.issueKey" class="suspect">
          <div class="ac-quality-issue-head">
            <span class="ac-quality-level">疑点</span>
            <span class="ac-quality-dimension">{{ chapterTitle(item.chapterId) }}</span>
            <button class="ac-text-link" type="button" @click="openChapter(item.chapterId)">打开章节</button>
          </div>
          <p class="ac-quality-message">{{ item.message }}</p>
          <template v-for="(quote, index) in item.quotes" :key="index">
            <blockquote v-if="quote">{{ quote }}</blockquote>
          </template>
          <div class="ac-quality-actions">
            <button
              class="ac-button ink-btn ink-btn-outline"
              type="button"
              :disabled="busy"
              @click="verdict(item, 'dismissed')"
            >
              合理剧情，驳回
            </button>
            <button
              class="ac-button ink-btn ink-btn-primary"
              type="button"
              :disabled="busy"
              @click="verdict(item, 'accepted')"
            >
              确认有问题
            </button>
          </div>
        </li>
      </ul>
      <div v-else class="ac-empty-state">
        <i class="fa-regular fa-circle-check" aria-hidden="true"></i>
        <h3>没有待裁决的疑点</h3>
        <p>
          已死亡人物再出场、同一时间点状态矛盾等问题会附两处原文列在这里；回忆、伪装等合理剧情驳回后不再提示。
        </p>
      </div>
    </template>

    <!-- 修改影响 -->
    <div v-else class="ac-canon-impact">
      <p class="ac-muted">
        改了前文的事实或设定后，选择那一章，列出后文可能受影响的章节。这是候选清单：可能漏，也可能多，处理范围由你决定。
      </p>
      <div class="ac-impact-picker">
        <el-select
          v-model="impactChapter"
          class="ink-select"
          popper-class="ink-select-popper"
          aria-label="选择修改过的章节"
          placeholder="选择修改过的章节"
          filterable
        >
          <el-option
            v-for="item in cardChapters"
            :key="item.id"
            :label="item.hasPrevious ? `${item.title}（有旧版可比）` : item.title"
            :value="item.id"
          />
        </el-select>
        <button
          class="ac-button ink-btn ink-btn-primary"
          :disabled="!impactChapter || impactLoading"
          @click="runImpact"
        >
          列出受影响章节
        </button>
      </div>
      <p v-if="!cardChapters.length" class="ac-muted">还没有建立索引的章节。</p>
      <template v-if="impact">
        <p v-if="!impact.hasPrevious" class="ac-index-note">
          这一章还没有可比对的旧版章节卡：索引建立后再修改这一章，才能比出前后差异。
        </p>
        <template v-else>
          <div class="ac-impact-changes">
            <h4>这一章的变化</h4>
            <ul class="ac-canon-knows">
              <li v-for="item in impact.changes.facts.changed" :key="`c${item.factId}`">
                事实改动：{{ item.before }} → {{ item.after }}
              </li>
              <li v-for="item in impact.changes.facts.removed" :key="`r${item.factId}`">
                事实删除：{{ item.text }}
              </li>
              <li v-for="item in impact.changes.facts.added" :key="`a${item.factId}`">
                新增事实：{{ item.text }}
              </li>
              <li v-for="(item, index) in impact.changes.states" :key="`s${index}`">
                {{ characterName(item.characterId) }}的{{ FIELD_LABELS[item.field] || item.field }}：{{
                  item.before
                }}
                →
                {{ item.after ?? '不再提及' }}
              </li>
            </ul>
            <p v-if="!impactChangeCount" class="ac-muted">事实与人物状态没有变化，后文不受影响。</p>
          </div>
          <div v-if="impact.affected.length" class="ac-impact-changes">
            <h4>可能受影响的后文（{{ impact.affected.length }} 章）</h4>
            <ul class="ac-quality-list">
              <li v-for="item in impact.affected" :key="item.chapterId" class="suspect">
                <div class="ac-quality-issue-head">
                  <span class="ac-quality-dimension">{{ chapterTitle(item.chapterId) }}</span>
                  <button class="ac-text-link" type="button" @click="openChapter(item.chapterId)">
                    打开章节
                  </button>
                </div>
                <p v-for="(reason, index) in item.reasons" :key="index" class="ac-quality-message">
                  {{ reason }}
                </p>
                <blockquote v-for="(quote, index) in item.quotes" :key="`q${index}`">{{ quote }}</blockquote>
              </li>
            </ul>
          </div>
          <p v-else-if="impactChangeCount" class="ac-muted">没有找到引用这些变化的后文章节。</p>
        </template>
      </template>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type {
  AgentChapterCardRecord,
  AgentCharacterState,
  AgentFact,
  AgentImpactReport,
  AgentIndexProgress,
  AgentStateAsOf,
  AgentSuspect,
  AgentThread
} from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
import {
  chapterTitleMap,
  formatStamp,
  stateLine,
  THREAD_KIND_LABELS,
  THREAD_STATUS_LABELS
} from '../canon-helpers'

type CanonView = 'states' | 'threads' | 'suspects' | 'impact'
const FIELD_LABELS: Record<string, string> = {
  realm: '境界',
  location: '所在地',
  item: '持有物',
  relation: '关系',
  goal: '目标',
  alive: '生死'
}
const props = defineProps<{ bookId: string }>()
const store = useAgentWorkspace()
const router = useRouter()
const view = ref<CanonView>('states')
const busy = ref(false)
const loadError = ref('')
const progress = ref<AgentIndexProgress | null>(null)
const states = ref<AgentCharacterState[]>([])
const threads = ref<AgentThread[]>([])
const suspects = ref<AgentSuspect[]>([])
const cards = ref<AgentChapterCardRecord[]>([])
const facts = ref<AgentFact[]>([])

const titles = computed(() => chapterTitleMap(store.availableReferences, props.bookId))
const chapterTitle = (id?: string | null) => (id ? titles.value.get(String(id)) || `章节 ${id}` : '未知章节')
const characterName = (id: string) => states.value.find(item => item.characterId === id)?.name || '人物'
const factText = (id: string) => facts.value.find(item => item.factId === id)?.text || id
/** 有有效章节卡的章节，按目录顺序 */
const cardChapters = computed(() => {
  const active = new Map(
    cards.value.filter(item => item.status === 'active').map(item => [item.chapterId, item])
  )
  return [...titles.value.entries()]
    .filter(([id]) => active.has(id))
    .map(([id, title]) => ({ id, title, hasPrevious: !!active.get(id)?.previousCard }))
})
const missingCount = computed(() =>
  progress.value ? Math.max(0, progress.value.total - progress.value.indexed) : 0
)
const percent = computed(() =>
  progress.value?.total ? Math.round((progress.value.indexed / progress.value.total) * 100) : 0
)
const indexSummary = computed(() => {
  const p = progress.value
  if (!p) return '正在读取索引状态…'
  const parts = [
    p.paused ? '本书索引已暂停' : '',
    p.stale ? `${p.stale} 章正文改动后待重新提取` : '',
    p.pendingEvents ? `${p.pendingEvents} 项变更排队中` : '',
    p.lastRunAt ? `上次索引 ${formatStamp(p.lastRunAt)}` : ''
  ].filter(Boolean)
  if (!p.indexed && p.total && !p.pendingEvents)
    parts.push('这本书还没有建立索引，点“补建索引”开始，每章调用一次索引模型')
  return parts.join(' · ') || '索引与正文同步'
})
const failures = computed(() => {
  const active = new Set(cards.value.filter(item => item.status === 'active').map(item => item.chapterId))
  return Object.entries(progress.value?.failed || {})
    .filter(([id, item]) => !active.has(id) && item.count >= 3)
    .map(([chapterId, item]) => ({ chapterId, message: item.message }))
})
const pendingEntities = computed(() => (progress.value?.pendingEntities || []).filter(Boolean))
const views = computed<Array<{ id: CanonView; label: string; count?: number }>>(() => [
  { id: 'states', label: '人物状态', count: states.value.length },
  { id: 'threads', label: '线索账本', count: threads.value.filter(item => item.status === 'open').length },
  { id: 'suspects', label: '疑点', count: suspects.value.length },
  { id: 'impact', label: '修改影响' }
])

async function load() {
  if (!props.bookId) return
  const bookId = props.bookId
  try {
    const [nextProgress, nextStates, nextThreads, nextSuspects, nextCards, nextFacts] = await Promise.all([
      store.canonQuery<AgentIndexProgress>('progress', bookId),
      store.canonQuery<AgentCharacterState[]>('states', bookId),
      store.canonQuery<AgentThread[]>('threads', bookId),
      store.canonQuery<AgentSuspect[]>('suspects', bookId),
      store.canonQuery<AgentChapterCardRecord[]>('cards', bookId),
      store.canonQuery<AgentFact[]>('facts', bookId)
    ])
    if (bookId !== props.bookId) return
    progress.value = nextProgress
    states.value = [...nextStates].sort((a, b) => b.appearances - a.appearances)
    threads.value = nextThreads
    suspects.value = nextSuspects
    cards.value = nextCards
    facts.value = nextFacts
    loadError.value = ''
  } catch (error) {
    loadError.value = `读取作品状态失败：${String(error)}`
  }
}

// 索引事件可能一章一条，合并成一次读取
let reloadTimer: ReturnType<typeof setTimeout> | undefined
function scheduleLoad() {
  clearTimeout(reloadTimer)
  reloadTimer = setTimeout(() => void load(), 600)
}
watch(() => store.indexTick, scheduleLoad)
onBeforeUnmount(() => clearTimeout(reloadTimer))

async function run(task: () => Promise<void>) {
  if (busy.value) return
  busy.value = true
  try {
    await task()
  } catch (error) {
    if (error !== 'cancel' && error !== 'close') ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
function buildMissing() {
  return run(async () => {
    await inkConfirm(
      `将为 ${missingCount.value} 章提取章节卡，每章调用一次索引模型，在后台进行。继续吗？`,
      '补建索引',
      { confirmButtonText: '开始', cancelButtonText: '取消' }
    )
    const { queued } = await store.indexRebuild(props.bookId, true)
    ElMessage.success(queued ? `已加入 ${queued} 章，索引在后台进行` : '没有需要补建的章节')
    await load()
  })
}
function onIndexCommand(command: string) {
  if (command === 'now')
    return run(async () => {
      await store.indexNow()
      ElMessage.success('已开始处理排队的变更')
    })
  if (command === 'rebuild')
    return run(async () => {
      await inkConfirm(
        `全书 ${progress.value?.total || 0} 章将重新提取章节卡，每章调用一次索引模型。通常只在换了索引模型后需要。继续吗？`,
        '全书重新提取',
        { confirmButtonText: '重新提取', cancelButtonText: '取消', type: 'warning' }
      )
      const { queued } = await store.indexRebuild(props.bookId, false)
      ElMessage.success(`已加入 ${queued} 章，索引在后台进行`)
      await load()
    })
  return run(async () => {
    const paused = !progress.value?.paused
    await store.setIndexPaused(props.bookId, paused)
    ElMessage.success(paused ? '已暂停本书索引，正文变更会保留到恢复后处理' : '已恢复本书索引')
    await load()
  })
}

// ---- 人物状态 ----
const selectedStateId = ref('')
const selectedState = computed(() => states.value.find(item => item.characterId === selectedStateId.value))
const asOfChapter = ref('')
const asOf = ref<AgentStateAsOf | null>(null)
const asOfLoading = ref(false)
const shownState = computed(() => (asOfChapter.value ? asOf.value?.state : selectedState.value?.latest))
const relationText = computed(() =>
  Object.entries(shownState.value?.relations || {})
    .map(([who, relation]) => `${who}：${relation}`)
    .join('；')
)
function selectState(id: string) {
  selectedStateId.value = id
  asOfChapter.value = ''
  asOf.value = null
}
watch(asOfChapter, async chapterId => {
  asOf.value = null
  if (!chapterId || !selectedStateId.value) return
  asOfLoading.value = true
  try {
    asOf.value = await store.canonQuery<AgentStateAsOf>('state', props.bookId, {
      characterId: selectedStateId.value,
      chapterId
    })
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    asOfLoading.value = false
  }
})

// ---- 线索账本 ----
const threadFilters: Array<{ id: AgentThread['status'] | 'all'; label: string }> = [
  { id: 'open', label: '未回收' },
  { id: 'paid', label: '已回收' },
  { id: 'dropped', label: '已放弃' },
  { id: 'all', label: '全部' }
]
const threadFilter = ref<AgentThread['status'] | 'all'>('open')
const filteredThreads = computed(() =>
  threads.value.filter(item => threadFilter.value === 'all' || item.status === threadFilter.value)
)
const selectedThreadId = ref('')
const selectedThread = computed(() => threads.value.find(item => item.threadId === selectedThreadId.value))
const threadForm = reactive<{ status: AgentThread['status']; note: string }>({ status: 'open', note: '' })
function selectThread(item: AgentThread) {
  selectedThreadId.value = item.threadId
  threadForm.status = item.status
  threadForm.note = item.note || ''
}
function saveThread() {
  const thread = selectedThread.value
  if (!thread) return
  return run(async () => {
    await store.overrideThread(props.bookId, thread.threadId, threadForm.status, threadForm.note.trim())
    ElMessage.success('线索标注已保存')
    await load()
  })
}

// ---- 疑点 ----
function verdict(item: AgentSuspect, decision: 'accepted' | 'dismissed') {
  return run(async () => {
    await store.saveVerdict(props.bookId, item.issueKey, decision)
    suspects.value = suspects.value.filter(entry => entry.issueKey !== item.issueKey)
    ElMessage.success(
      decision === 'dismissed' ? '已驳回，下次不再提示' : '已确认，可以让 Agent 修订或自己修改'
    )
  })
}
async function openChapter(chapterId: string) {
  await router.push({
    name: 'Writing',
    params: { bookId: props.bookId },
    query: { from: 'agent', chapterId }
  })
}

// ---- 修改影响 ----
const impactChapter = ref('')
const impact = ref<AgentImpactReport | null>(null)
const impactLoading = ref(false)
const impactChangeCount = computed(() => {
  const changes = impact.value?.changes
  if (!changes) return 0
  return (
    changes.facts.changed.length +
    changes.facts.removed.length +
    changes.facts.added.length +
    changes.states.length
  )
})
async function runImpact() {
  if (!impactChapter.value) return
  impactLoading.value = true
  try {
    impact.value = await store.impactReport(props.bookId, impactChapter.value)
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    impactLoading.value = false
  }
}

// 放在最后：立即执行时要用到上面声明的各项状态
watch(
  () => props.bookId,
  () => {
    selectedStateId.value = ''
    selectedThreadId.value = ''
    impact.value = null
    impactChapter.value = ''
    void load()
  },
  { immediate: true }
)
</script>
