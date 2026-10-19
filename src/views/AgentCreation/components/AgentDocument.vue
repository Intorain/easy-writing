<template>
  <section
    v-if="artifact"
    class="ac-document"
    :class="{ 'is-expanded': expanded }"
    aria-label="内容编辑区"
  >
    <header class="ac-document-header">
      <div>
        <h2>{{ artifact.title }}</h2>
        <p>{{ artifact.bookTitle || '独立会话' }} / {{ artifactLabel }}</p>
      </div>
      <div class="ac-actions">
        <button
          class="ac-icon-button"
          :aria-label="expanded ? '恢复面板宽度' : '展开阅读'"
          :title="expanded ? '恢复宽度' : '展开阅读'"
          @click="$emit('expand')"
        >
          <i :class="expanded ? 'fa-solid fa-compress' : 'fa-solid fa-expand'" aria-hidden="true"></i></button
        ><button class="ac-icon-button" aria-label="关闭正文面板" @click="close">
          <i class="fa-solid fa-xmark" aria-hidden="true"></i>
        </button>
      </div>
    </header>
    <div class="ac-document-tabs">
      <div class="ac-tabs" role="tablist" aria-label="成果显示方式">
        <template v-if="markdownDocument">
          <button
            role="tab"
            :aria-selected="documentMode === 'preview'"
            :class="{ active: documentMode === 'preview' }"
            @click="documentMode = 'preview'"
          >
            预览
          </button>
          <button
            role="tab"
            :aria-selected="documentMode === 'edit'"
            :class="{ active: documentMode === 'edit' }"
            @click="documentMode = 'edit'"
          >
            {{ artifact.applied ? '源码' : '编辑' }}
          </button> </template
        ><button v-else class="active" role="tab" aria-selected="true">
          {{ artifactLabel }}
        </button>
      </div>
      <span class="ac-state-label" :class="{ success: artifact.applied }">{{ stateLabel }}</span>
    </div>
    <div v-if="artifact.kind === 'skill' && artifact.skillDraft" class="ac-skill-candidate">
      <p class="ac-index-note">
        {{
          artifact.applied
            ? '技能已保存。首次运行脚本时会在对话中确认当前版本。'
            : artifact.skillCopy
              ? '此候选保存为自定义副本，原内置技能保留。可继续对话修改，也可以在这里编辑。'
              : '候选已保留在当前会话中。可继续对话修改，也可以在这里编辑；点击“保存技能”后才写入技能库。'
        }}
      </p>
      <AgentSkillFields
        :model-value="artifact.skillDraft"
        :readonly="artifact.applied || saving"
        :preserved-files="artifact.preservedFiles"
        @update:model-value="changeSkillDraft"
      />
    </div>
    <AgentBookPlan
      v-else-if="artifact.kind === 'book_plan' && artifact.bookPlan"
      :plan="artifact.bookPlan"
      :progress="artifact.progress"
      :selected-story="artifact.selectedDirection"
      :readonly="artifact.applied || saving"
      @change="changePlan"
    />
    <AgentSelectionCards
      v-else-if="artifact.kind === 'selection' && artifact.selectionCards"
      :cards="artifact.selectionCards"
      :disabled="store.submitting || store.active?.status === 'running'"
      @use="useSelection"
    />
    <AgentComparison
      v-else-if="artifact.kind === 'comparison' && artifact.rows"
      :rows="artifact.rows"
      @use="useComparison"
    />
    <AgentNameList
      v-else-if="artifact.kind === 'names' && artifact.candidates"
      :candidates="artifact.candidates"
      :check-summary="artifact.nameCheckSummary"
    />
    <AgentCoverBrief
      v-else-if="artifact.kind === 'cover'"
      :artifact="artifact"
      :readonly="saving"
      @change="changeCoverPrompt"
      @covered="onCovered"
    />
    <AgentMaterialFields
      v-else-if="artifact.kind === 'material' && artifact.materialFields"
      :fields="artifact.materialFields"
      :readonly="artifact.applied || saving"
      @change="changeFields"
    />
    <!-- eslint-disable vue/no-v-html -- 与对话共用安全 Markdown 解析器，禁用原始 HTML。 -->
    <div
      v-else-if="markdownDocument && documentMode === 'preview'"
      class="ac-document-preview ac-document-markdown"
      aria-label="内容预览"
      v-html="renderMarkdown(artifact.content)"
    ></div>
    <!-- eslint-enable vue/no-v-html -->
    <textarea
      v-else
      v-model="artifact.content"
      @input="queueSave"
      class="ac-document-editor"
      :readonly="artifact.applied || artifact.partial || saving"
      :aria-label="prose ? '编辑正文' : '编辑内容'"
      spellcheck="false"
    ></textarea>
    <p v-if="artifact.partial" class="ac-document-banner">{{ writing ? '正在写，写完会自动核对。' : '写作已停止，已生成内容已保留。' }}</p>
    <footer class="ac-document-footer">
      <div>
        <span><i class="fa-regular fa-clock" aria-hidden="true"></i>{{ saveHint }}</span
        ><small v-if="artifact.kind === 'skill'"
          >{{ artifact.skillDraft?.files.length || 0 }} 个附加文件</small
        >
        <small v-else>{{ artifact.content.replace(/\s/g, '').length }} 字</small>
      </div>
      <div class="ac-actions">
        <BreakdownSaveButton v-if="artifact.kind === 'breakdown_report' && artifact.projectId" :project-id="artifact.projectId" :before-save="() => store.saveArtifact()" />
        <template v-if="artifact.applied"
          ><button
            v-if="artifact.kind === 'chapter' || artifact.kind === 'intro'"
            class="ac-button ink-btn ink-btn-outline"
            :disabled="saving"
            @click="undo"
          >
            撤销</button
          ><button v-if="artifact.bookId" class="ac-button ink-btn ink-btn-outline" @click="openBook">
            打开作品</button
          ><button
            v-if="artifact.kind === 'skill'"
            class="ac-button ink-btn ink-btn-outline"
            @click="router.push('/agent-creation/skills')"
          >
            查看技能
          </button>
          <button class="ac-button ink-btn ink-btn-primary" @click="close">完成</button></template
        ><template
          v-else-if="
            ['chapter', 'material', 'book_plan', 'skill'].includes(artifact.kind) ||
            (artifact.kind === 'intro' && artifact.bookId)
          "
          ><button
            class="ac-button ink-btn ink-btn-outline"
            @click="artifact.kind !== 'chapter' ? close() : discard()"
          >
            {{ artifact.kind !== 'chapter' ? '继续讨论' : '放弃' }}</button
          ><button
            class="ac-button ink-btn ink-btn-primary"
            :disabled="
              saving ||
              artifact.partial ||
              !artifact.content.trim() ||
              (artifact.kind === 'book_plan' && artifact.progress?.buildable !== true)
            "
            @click="apply"
          >
            {{
              saving
                ? '正在保存…'
                : artifact.kind === 'skill'
                  ? artifact.skillCopy
                    ? '保存自定义副本'
                    : '保存技能'
                  : artifact.kind === 'book_plan'
                    ? artifact.bookId
                      ? '更新这本书'
                      : '创建作品'
                    : '保存到作品'
            }}
          </button></template
        ><button v-else class="ac-button ink-btn ink-btn-primary" @click="close">返回对话</button>
      </div>
    </footer>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch, onBeforeUnmount } from 'vue'
import { ElMessage } from 'element-plus'
import { useRouter } from 'vue-router'
import type { AgentArtifact, AgentSkillDraft } from '@/types/agent-creation'
import AgentSkillFields from './AgentSkillFields.vue'
import AgentMaterialFields from './AgentMaterialFields.vue'
import AgentBookPlan from './AgentBookPlan.vue'
import AgentSelectionCards from './AgentSelectionCards.vue'
import AgentComparison from './AgentComparison.vue'
import AgentNameList from './AgentNameList.vue'
import AgentCoverBrief from './AgentCoverBrief.vue'
import { renderMarkdown } from '@/utils/markdown'
import type { AgentBookPlan as BookPlan } from '@/types/agent-creation'
import BreakdownSaveButton from '@/components/BreakdownSaveButton.vue'
import { inkConfirm } from '@/utils/ink-confirm'
import { useAgentWorkspace } from '../use-agent-workspace'

defineProps<{ expanded?: boolean }>()
defineEmits<{ expand: [] }>()
const store = useAgentWorkspace()
const router = useRouter()
const saving = ref(false)
const artifact = computed(() => store.openedArtifact)
const writing = computed(() => !!artifact.value && store.isArtifactWriting(artifact.value))
const documentMode = ref<'preview' | 'edit'>('preview')
/** 正文类成果（章节、短篇、试写）：只有编辑框，没有页签 */
const prose = computed(() => ['chapter', 'short', 'draft'].includes(artifact.value?.kind || ''))
// 结构化成果保留各自编辑器；小说正文始终按原文展示，方案、报告等文档默认预览。
const markdownDocument = computed(() => {
  const value = artifact.value
  if (!value || ['chapter', 'short', 'draft', 'intro'].includes(value.kind)) return false
  const structured = {
    skill: !!value.skillDraft,
    book_plan: !!value.bookPlan,
    selection: !!value.selectionCards,
    comparison: !!value.rows,
    names: !!value.candidates,
    cover: true,
    material: !!value.materialFields
  }
  return !structured[value.kind as keyof typeof structured]
})
const KIND_LABELS: Partial<Record<AgentArtifact['kind'], string>> = {
  skill: '技能候选',
  book_plan: '开书方案',
  draft: '试写草稿',
  chapter: '章节正文',
  beats: '章节拍子',
  selection: '选题卡',
  comparison: '仿书对照方案',
  structure: '短篇结构卡',
  names: '取名候选',
  intro: '作品简介',
  cover: '封面方案',
  report: '报告',
  breakdown_report: '拆书报告'
}
const artifactLabel = computed(() => {
  const value = artifact.value
  if (!value) return ''
  if (value.kind === 'material')
    return (
      { character: '人物资料', outline: '作品大纲', world_setting: '世界设定' }[value.materialKind!] ||
      '作品资料'
    )
  if (value.kind === 'short') return value.variant ? `短篇稿 · ${value.variant}` : '短篇稿'
  return KIND_LABELS[value.kind] || '创作方案'
})
const stateLabel = computed(() => {
  const value = artifact.value
  if (!value) return ''
  if (value.partial) return writing.value ? '正在写…' : '未完成 · 已停止'
  if (value.kind === 'book_plan') return value.applied ? '已创建' : value.progress?.buildable ? '可以创建' : '拟定中'
  if (value.applied) return value.kind === 'skill' ? '已保存到技能库' : value.kind === 'chapter' ? '已存为新版本' : '已保存到作品'
  if (value.kind === 'chapter') return '未写入作品'
  return ['breakdown_report', 'report', 'outline', 'beats', 'selection', 'comparison', 'structure', 'short', 'draft', 'names', 'cover'].includes(value.kind)
    ? '已保留'
    : '待保存'
})
const saveHint = computed(() =>
  artifact.value?.kind === 'skill'
    ? artifact.value.applied
      ? '已保存到技能库'
      : '当前仅为技能候选'
    : artifact.value?.kind === 'book_plan'
      ? artifact.value.applied
        ? '作品已创建，书名和资料以后仍可修改'
        : artifact.value.progress?.buildable
          ? '方案已保存，可以创建'
          : artifact.value.progress?.blocking.length
            ? `创建作品还需：${artifact.value.progress.blocking.join('、')}`
            : '方案会自动保存，继续对话即可完善'
      : artifact.value?.applied
        ? artifact.value.kind === 'chapter'
          ? '已保存，撤销可回到上一版；历史在章节历史里'
          : artifact.value.kind === 'intro'
            ? '已写进作品简介，撤销可恢复原简介'
            : '已保存到作品资料'
        : artifact.value?.kind === 'chapter'
          ? '还没写进作品，可以先改再保存'
          : artifact.value?.kind === 'material' || (artifact.value?.kind === 'intro' && artifact.value.bookId)
            ? '候选已保留，保存到作品后正式生效'
            : '保存在当前会话中'
)
watch(
  () => artifact.value?.id,
  () => {
    documentMode.value = 'preview'
  }
)
let saveTimer: ReturnType<typeof setTimeout> | undefined
let pendingArtifactId: string | undefined
function queueSave() {
  store.markArtifactDirty()
  pendingArtifactId = artifact.value?.id
  clearTimeout(saveTimer)
  saveTimer = setTimeout(() => {
    saveTimer = undefined
    void store.saveArtifact(pendingArtifactId).catch(error => ElMessage.error(String(error)))
  }, 400)
}
function changeCoverPrompt(value: string) {
  if (!artifact.value) return
  artifact.value.content = value
  queueSave()
}
function onCovered(updated: AgentArtifact) {
  if (!artifact.value || artifact.value.id !== updated.id) return
  artifact.value.coverUrl = updated.coverUrl
  artifact.value.revision = updated.revision
  ElMessage.success(artifact.value.bookId ? '封面已生成并保存到作品' : '封面已生成')
}
function changeFields(fields: NonNullable<AgentArtifact['materialFields']>) {
  if (!artifact.value) return
  artifact.value.materialFields = fields
  queueSave()
}
function changePlan(plan: BookPlan) {
  if (!artifact.value) return
  artifact.value.bookPlan = plan
  // 编辑后的建书资格由宿主重新计算，不能沿用上一版的进度。
  artifact.value.progress = undefined
  queueSave()
}
function changeSkillDraft(draft: AgentSkillDraft) {
  if (!artifact.value || artifact.value.applied) return
  artifact.value.skillDraft = draft
  queueSave()
}
async function openBook() {
  const bookId = artifact.value?.bookId
  if (bookId)
    await router.push({
      name: 'Writing',
      params: { bookId },
      query: artifact.value?.chapterId ? { from: 'agent', chapterId: artifact.value.chapterId } : {}
    })
}
onBeforeUnmount(() => {
  if (saveTimer) {
    clearTimeout(saveTimer)
    void store.saveArtifact(pendingArtifactId).catch(error => ElMessage.error(String(error)))
  }
})
// 选定方向交回对话：填进输入框由作者确认后发送，不自动建书
async function useComparison(target: 'plan' | 'outline') {
  const title = artifact.value?.title || '对照方案'
  store.draft =
    target === 'plan'
      ? `按《${title}》准备开书，只借结构与写法，人物和设定都用我自己的。`
      : `按《${title}》规划前 10 章章纲，每章写清给出的期待与兑现的期待。`
  await close()
  ElMessage.info('已填入输入框，确认后发送')
}
async function useSelection(index: number) {
  if (artifact.value) await store.selectDirection(artifact.value.id, index)
}
async function close() {
  clearTimeout(saveTimer)
  try {
    await store.saveArtifact()
  } catch (error) {
    ElMessage.error(String(error))
    return
  }
  store.openedArtifactId = null
}
async function apply() {
  if (saving.value) return
  saving.value = true
  try {
    await store.applyArtifact()
    ElMessage.success(
      artifact.value?.kind === 'skill'
        ? '技能已保存；脚本运行仍需单独确认'
        : artifact.value?.kind === 'book_plan'
          ? '作品已保存'
          : '已保存到作品'
    )
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    saving.value = false
  }
}
async function undo() {
  if (saving.value) return
  saving.value = true
  try {
    await store.undoArtifact()
    ElMessage.success('已撤销，正文回到上一版')
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    saving.value = false
  }
}
async function discard() {
  try {
    await inkConfirm('放弃这份稿子？作品正文保持不变。', '放弃稿子', {
      confirmButtonText: '放弃',
      cancelButtonText: '继续编辑',
      type: 'warning'
    })
    close()
  } catch {
    /* 用户取消放弃，保留当前稿子。 */
  }
}
</script>
