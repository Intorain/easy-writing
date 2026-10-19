<template>
  <section class="ac-canon" aria-label="文风">
    <p v-if="loadError" class="ac-index-note warning" role="alert">{{ loadError }}</p>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>文风样本</h3>
          <p>
            只从你确认是亲手写的章节学习文风。导入、粘贴或 AI 生成的章节默认不计入，以免把别人的写法当成你的。
          </p>
        </div>
        <button
          class="ac-button ink-btn ink-btn-primary"
          :disabled="busy || !samplesDirty"
          @click="saveSamples"
        >
          保存样本
        </button>
      </header>
      <div class="ac-sample-toolbar">
        <label class="ac-search"
          ><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i
          ><input v-model="query" aria-label="筛选章节" placeholder="筛选章节..."
        /></label>
        <span class="ac-muted">已选 {{ selected.length }} 章</span>
        <button v-if="selected.length" class="ac-text-link" type="button" @click="selected = []">
          全部取消
        </button>
      </div>
      <div class="ac-sample-list" role="group" aria-label="用于学习文风的章节">
        <el-checkbox
          v-for="item in filteredChapters"
          :key="item.id"
          :model-value="selected.includes(item.id)"
          @change="toggle(item.id)"
        >
          {{ item.title }}<small v-if="!indexed.has(item.id)" class="ac-muted">（未索引，暂不计字数）</small
          ><small v-else-if="aiSpans[item.id]" class="ac-muted">（含 AI 插入 {{ aiSpans[item.id] }} 处，这些段落不计入）</small>
        </el-checkbox>
        <p v-if="!chapters.length" class="ac-small-empty">这本书还没有章节。</p>
      </div>
      <p class="ac-muted ac-form-note">
        样本从已建立索引的章节正文里取；未索引的章节先到“作品状态”页补建索引，再回来保存一次。
      </p>
    </div>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>风格档案</h3>
          <p>按样本统计句长、段长、对白比例、标点习惯和高频短语。</p>
        </div>
        <button class="ac-button ink-btn ink-btn-outline" :disabled="busy" @click="rebuild">重新计算</button>
      </header>
      <p v-if="!style" class="ac-small-empty">还没有风格档案，保存样本后自动计算。</p>
      <template v-else>
        <p class="ac-index-note" :class="{ warning: style.insufficient }">
          {{
            style.insufficient
              ? `样本 ${style.sampleChars} 字，统计参考较少；已确认片段仍可按场景选作表达示例。`
              : `样本 ${style.sampleChars} 字；统计只描述样本，写作要求以下方作者锁定内容为准。`
          }}
        </p>
        <dl v-if="!style.insufficient" class="ac-profile-summary">
          <dt>句子</dt>
          <dd>平均 {{ style.metrics.sentenceMean }} 字，长短差 {{ style.metrics.sentenceStd }}</dd>
          <dt>段落</dt>
          <dd>平均 {{ style.metrics.paragraphMean }} 字</dd>
          <dt>对白</dt>
          <dd>约占 {{ Math.round(style.metrics.dialogueRatio * 100) }}%</dd>
          <dt>标点</dt>
          <dd>
            每千字叹号 {{ style.metrics.exclamationPer1k }}、省略号 {{ style.metrics.ellipsisPer1k }}、破折号
            {{ style.metrics.dashPer1k }}
          </dd>
          <template v-if="style.phrases.length">
            <dt>高频短语</dt>
            <dd class="ac-phrase-list">
              <span v-for="item in style.phrases" :key="item.phrase"
                >{{ item.phrase }}<small>{{ item.count }}</small></span
              >
            </dd>
          </template>
        </dl>
      </template>
    </div>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>写作时注入的风格要点</h3>
          <p>填写并锁定你希望采用的叙述口气、视角和对白习惯；统计不会自动变成写作规则。</p>
        </div>
        <span v-if="style?.locked" class="ac-state-label">已锁定</span>
      </header>
      <textarea
        v-model="points"
        class="ink-input ac-style-points"
        rows="4"
        maxlength="2000"
        placeholder="如：短句为主，动作开头；对白不加“说道”；少用比喻和形容词堆叠"
      ></textarea>
      <div class="ac-actions ac-style-actions">
        <button
          v-if="style?.locked"
          class="ac-button ink-btn ink-btn-outline"
          :disabled="busy"
          @click="lock('')"
        >
          解除锁定
        </button>
        <button
          class="ac-button ink-btn ink-btn-primary"
          :disabled="busy || !points.trim()"
          @click="lock(points)"
        >
          锁定这些要点
        </button>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import type { AgentChapterCardRecord, AgentStyleProfile, AgentStyleView } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
import { chapterTitleMap } from '../canon-helpers'

const props = defineProps<{ bookId: string }>()
const store = useAgentWorkspace()
const busy = ref(false)
const loadError = ref('')
const query = ref('')
const style = ref<AgentStyleProfile | null>(null)
const savedSamples = ref<string[]>([])
const selected = ref<string[]>([])
const points = ref('')
const indexed = ref(new Set<string>())
const aiSpans = ref<Record<string, number>>({})

const chapters = computed(() =>
  [...chapterTitleMap(store.availableReferences, props.bookId).entries()].map(([id, title]) => ({
    id,
    title
  }))
)
const filteredChapters = computed(() =>
  chapters.value.filter(item => item.title.includes(query.value.trim()))
)
const samplesDirty = computed(
  () =>
    selected.value.length !== savedSamples.value.length ||
    selected.value.some(id => !savedSamples.value.includes(id))
)

function applyStyle(next: AgentStyleProfile | null, lockedPoints?: string | null) {
  style.value = next
  points.value = lockedPoints ?? (next?.locked ? next.points : '')
}
async function load() {
  const bookId = props.bookId
  if (!bookId) return
  try {
    const [view, cards] = await Promise.all([
      store.canonQuery<AgentStyleView>('style', bookId),
      store.canonQuery<AgentChapterCardRecord[]>('cards', bookId)
    ])
    if (bookId !== props.bookId) return
    savedSamples.value = view.samples
    selected.value = [...view.samples]
    aiSpans.value = view.aiSpans || {}
    indexed.value = new Set(cards.filter(item => item.status === 'active').map(item => item.chapterId))
    applyStyle(view.style, view.lock?.points)
    loadError.value = ''
  } catch (error) {
    loadError.value = `读取文风档案失败：${String(error)}`
  }
}

async function run(task: () => Promise<void>) {
  if (busy.value) return
  busy.value = true
  try {
    await task()
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
function toggle(id: string) {
  selected.value = selected.value.includes(id)
    ? selected.value.filter(item => item !== id)
    : [...selected.value, id]
}
function saveSamples() {
  return run(async () => {
    // 按目录顺序保存，避免勾选顺序影响统计
    const ordered = chapters.value.map(item => item.id).filter(id => selected.value.includes(id))
    const record = await store.confirmSamples(props.bookId, ordered)
    savedSamples.value = ordered
    selected.value = [...ordered]
    applyStyle(record.style, style.value?.locked ? points.value : null)
    ElMessage.success(
      ordered.length ? '样本已保存，风格档案已重新计算' : '已清空样本，写作时不再注入风格要点'
    )
  })
}
function rebuild() {
  return run(async () => {
    applyStyle(await store.rebuildStyle(props.bookId), style.value?.locked ? points.value : null)
    ElMessage.success('风格档案已重新计算')
  })
}
function lock(value: string) {
  return run(async () => {
    const next = await store.lockStyle(props.bookId, value.trim())
    applyStyle(next, value.trim() || null)
    ElMessage.success(value.trim() ? '已锁定，写作时以这些要点为准' : '已解除锁定，改用自动统计的要点')
  })
}
watch(
  () => props.bookId,
  () => {
    query.value = ''
    void load()
  },
  { immediate: true }
)
</script>
