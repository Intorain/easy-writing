<template>
  <section class="ac-manager ac-skills-manager" aria-label="Skills 管理">
    <div class="ac-manager-heading">
      <div>
        <h2>创作技能</h2>
        <p>管理创作方法与扩展能力</p>
      </div>
      <div class="ac-actions">
        <button class="ac-button ink-btn ink-btn-outline" @click="emit('create')">
          <i class="fa-solid fa-wand-magic-sparkles" aria-hidden="true"></i>AI 创建
        </button>
        <el-dropdown trigger="click" popper-class="ac-conversation-menu" @command="onImport">
          <button class="ac-button ink-btn ink-btn-outline">
            <i class="fa-solid fa-arrow-up-from-bracket" aria-hidden="true"></i>导入技能
          </button>
          <template #dropdown>
            <el-dropdown-menu>
              <el-dropdown-item command="folder">导入技能文件夹（可含脚本）</el-dropdown-item>
              <el-dropdown-item command="file">导入 SKILL.md 文本</el-dropdown-item>
            </el-dropdown-menu>
          </template> </el-dropdown
        ><button class="ac-button ink-btn ink-btn-primary" @click="edit()">
          <i class="fa-solid fa-plus" aria-hidden="true"></i>新建技能
        </button>
      </div>
    </div>
    <div class="ac-manager-content has-detail">
      <div class="ac-manager-list">
        <label class="ac-search"
          ><i class="fa-solid fa-magnifying-glass" aria-hidden="true"></i
          ><input v-model="query" aria-label="搜索技能" placeholder="搜索技能..."
        /></label>
        <div class="ac-filter-buttons" role="group" aria-label="技能来源">
          <button
            v-for="item in sources"
            :key="item.id"
            :aria-pressed="filter === item.id"
            :class="{ active: filter === item.id }"
            @click="filter = item.id"
          >
            {{ item.label }}
          </button>
        </div>
        <div v-if="filtered.length" class="ac-skill-list" aria-label="技能列表" tabindex="0">
          <div
            v-for="skill in filtered"
            :key="skill.id"
            class="ac-skill-row"
            :class="{ selected: selectedId === skill.id }"
          >
            <button class="ac-skill-select" @click="selectedId = skill.id">
              <i :class="skill.icon" aria-hidden="true"></i
              ><span
                ><strong>{{ skill.name }}</strong
                ><small>{{ markdownToPlainText(skill.description) }}</small></span
              ></button
            ><span class="ac-source-tag">{{ sourceLabel(skill.source) }}</span
            ><el-switch
              :model-value="skill.enabled"
              :aria-label="`${skill.enabled ? '停用' : '启用'} ${skill.name}`"
              @update:model-value="toggle(skill)"
            />
          </div>
        </div>
        <div v-if="!filtered.length" class="ac-empty-state">
          <i class="fa-solid fa-puzzle-piece" aria-hidden="true"></i>
          <h3>没有找到匹配的技能</h3>
          <p>换个关键词，或创建自己的创作方法。</p>
          <button class="ac-button ink-btn ink-btn-outline" @click="resetFilters">重置筛选</button>
        </div>
      </div>
      <aside v-if="selected" class="ac-detail ac-skill-detail" aria-label="技能详情">
        <div class="ac-skill-identity">
          <i :class="selected.icon" aria-hidden="true"></i>
          <div>
            <h3>{{ selected.name }}</h3>
            <p>{{ sourceLabel(selected.source) }}技能</p>
          </div>
        </div>
        <p class="ac-skill-description">{{ markdownToPlainText(selected.description) }}</p>
        <p v-if="selected.builtinLatest" class="ac-index-note">
          内置版本已更新，你改过的说明仍然保留；想用新版可以“恢复默认”。
        </p>
        <dl>
          <template v-if="selected.kind"
            ><dt>技能类型</dt>
            <dd>{{ kindLabel(selected.kind) }}</dd></template
          >
          <template v-if="selected.kind === 'method'"
            ><dt>写作方法</dt>
            <dd>
              {{
                selected.replaces && selected.replaces !== 'chapter-writing'
                  ? `替换「${selected.replaces}」`
                  : '章节写作'
              }}
            </dd></template
          >
          <dt>访问范围</dt>
          <dd>当前引用的作品与资料</dd>
          <dt>使用方式</dt>
          <dd>在对话中指定，或由 Agent 自动选择</dd>
        </dl>
        <AgentSkillScripts
          v-if="selected.filePath"
          :key="`${selected.id}:${selected.revision}:${selected.enabled}`"
          :skill="selected"
          @edit="edit(selected)"
        />
        <div class="ac-detail-actions">
          <button class="ac-button ink-btn ink-btn-outline" @click="emit('create', selected.id)">
            让 AI 修改
          </button>
          <button class="ac-button ink-btn ink-btn-outline" @click="instructionsVisible = true">
            <i class="fa-regular fa-file-lines" aria-hidden="true"></i>查看说明</button
          ><button class="ac-button ink-btn ink-btn-outline" @click="toggle(selected)">
            <i class="fa-solid fa-power-off" aria-hidden="true"></i
            >{{ selected.enabled ? '停用' : '启用' }}</button
          ><button
            class="ac-button ink-btn ink-btn-outline"
            title="复制成技能文件夹，可以分享给别人"
            @click="exportSkill(selected)"
          >
            <i class="fa-solid fa-share-from-square" aria-hidden="true"></i>导出
          </button>
        </div>
      </aside>
      <aside v-else class="ac-detail ac-empty-state">
        <i class="fa-solid fa-puzzle-piece" aria-hidden="true"></i>
        <h3>选择一个技能</h3>
        <p>查看说明、使用范围与运行依赖。</p>
      </aside>
    </div>
    <input ref="fileInput" type="file" accept=".md,.txt" hidden @change="importSkill" />
    <AgentSkillReview
      v-model:visible="reviewVisible"
      :preview="folderPreview"
      mode="install"
      :busy="installing"
      @confirm="installFolder"
    />
    <AgentSkillEditor
      v-model:visible="formVisible"
      :initial="form"
      :copy="editingBuiltin"
      :preserved-files="preservedFiles"
      @saved="afterSaved"
    />
    <EwModal
      v-model:visible="instructionsVisible"
      :title="selected?.name || '技能说明'"
      width="620px"
      custom-class="ac-dialog"
      manage-focus
    >
      <!-- eslint-disable vue/no-v-html -- 共用解析器禁用原始 HTML 并校验链接协议。 -->
      <div
        class="ac-instructions ac-skill-markdown"
        v-html="renderMarkdown(selected?.instructions || '')"
      ></div>
      <!-- eslint-enable vue/no-v-html -->
      <template #footer>
        <div v-if="selected" class="ac-skill-dialog-actions">
          <button v-if="selected.source !== 'builtin'" class="ac-text-link" @click="remove">删除技能</button>
          <div class="ac-actions">
            <button class="ac-button ink-btn ink-btn-outline" @click="instructionsVisible = false">
              关闭
            </button>
            <button class="ac-button ink-btn ink-btn-outline" @click="edit(selected)">编辑技能</button>
            <button class="ac-button ink-btn ink-btn-primary" :disabled="!selected.enabled" @click="useSkill">
              在对话中使用
            </button>
          </div>
        </div>
      </template></EwModal
    >
  </section>
</template>

<script setup lang="ts">
import EwModal from '@/components/EwModal/index.vue'
import { computed, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import { blankSkillDraft } from '@/agent/skill-draft'
import AgentSkillEditor from './AgentSkillEditor.vue'
import { markdownToPlainText, renderMarkdown } from '@/utils/markdown'
import type { AgentSkill, AgentSkillDraft, AgentSkillPreview } from '@/types/agent-creation'
import { agentCommand } from '@/agent/client'
import { useAgentWorkspace } from '../use-agent-workspace'
import AgentSkillScripts from './AgentSkillScripts.vue'
import AgentSkillReview from './AgentSkillReview.vue'

const emit = defineEmits<{ use: []; create: [skillId?: string] }>()
const store = useAgentWorkspace()
const query = ref('')
const filter = ref('all')
const selectedId = ref('')
const fileInput = ref<HTMLInputElement>()
const formVisible = ref(false)
const editingBuiltin = ref(false)
const preservedFiles = ref<string[]>([])
const instructionsVisible = ref(false)
const sources = [
  { id: 'all', label: '全部' },
  { id: 'builtin', label: '内置' },
  { id: 'custom', label: '自定义' },
  { id: 'third-party', label: '第三方' }
]
const sourceLabel = (value: AgentSkill['source']) =>
  ({ builtin: '内置', custom: '自定义', 'third-party': '第三方' })[value]
const kindLabel = (value: NonNullable<AgentSkill['kind']>) =>
  ({ method: '写作方法（参与任务规则集）', style: '文风（辅助写作）', check: '检查', tool: '工具流程' })[
    value
  ]
// ---- 技能文件夹：安装前过目脚本全文，导出供分享 ----
const reviewVisible = ref(false)
const folderPreview = ref<AgentSkillPreview | null>(null)
const folderPath = ref('')
const installing = ref(false)
async function pickDirectory(title: string) {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const picked = await open({ directory: true, multiple: false, title })
  return typeof picked === 'string' ? picked : ''
}
async function onImport(command: string) {
  if (command === 'file') {
    fileInput.value?.click()
    return
  }
  try {
    const path = await pickDirectory('选择技能文件夹（里面有 SKILL.md）')
    if (!path) return
    folderPreview.value = await agentCommand<AgentSkillPreview>('skill_inspect', { path })
    folderPath.value = path
    reviewVisible.value = true
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function installFolder() {
  if (!folderPreview.value || installing.value) return
  installing.value = true
  try {
    const saved = await agentCommand<AgentSkill>('skill_install', {
      path: folderPath.value,
      revision: folderPreview.value.revision
    })
    reviewVisible.value = false
    await store.reload()
    filter.value = 'all'
    query.value = ''
    selectedId.value = saved.id
    ElMessage.success('技能已安装')
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    installing.value = false
  }
}
async function exportSkill(skill: AgentSkill) {
  try {
    const dest = await pickDirectory('选择导出位置')
    if (!dest) return
    const { path } = await agentCommand<{ path: string }>('skill_export', { id: skill.id, dest })
    ElMessage.success(`已导出到 ${path}`)
  } catch (error) {
    ElMessage.error(String(error))
  }
}
const filtered = computed(() =>
  store.skills.filter(
    item =>
      (filter.value === 'all' || item.source === filter.value) &&
      `${item.name}${item.description}`.includes(query.value.trim())
  )
)
const selected = computed(() => filtered.value.find(item => item.id === selectedId.value))
const form = ref<AgentSkillDraft>(blankSkillDraft())
watch([filter, query], () => {
  if (!selected.value) selectedId.value = filtered.value[0]?.id || ''
})
async function toggle(skill: AgentSkill) {
  try {
    await store.setSkillEnabled(skill.id, !skill.enabled)
    if (skill.enabled && store.selectedSkillId === skill.id) store.selectedSkillId = null
  } catch (error) {
    ElMessage.error(String(error))
  }
}
function resetFilters() {
  query.value = ''
  filter.value = 'all'
}
async function edit(skill?: AgentSkill) {
  try {
    const preview = skill ? await agentCommand<AgentSkillPreview>('skill_read', { id: skill.id }) : null
    form.value = preview
      ? {
          id: skill!.id,
          name: preview.name,
          description: preview.description,
          instructions: preview.instructions,
          kind: preview.kind,
          replaces: preview.replaces,
          revision: preview.revision,
          files: preview.files
            .filter(file => file.editable && file.content !== null)
            .map(file => ({ path: file.path, content: file.content! }))
        }
      : blankSkillDraft()
    editingBuiltin.value = !!preview?.readOnly
    preservedFiles.value = preview?.files.filter(file => !file.editable).map(file => file.path) || []
    instructionsVisible.value = false
    formVisible.value = true
  } catch (error) {
    ElMessage.error(String(error))
  }
}
function afterSaved(saved: AgentSkill) {
  query.value = ''
  filter.value = 'all'
  selectedId.value = saved.id
}
async function importSkill(event: Event) {
  const target = event.target as HTMLInputElement
  const file = target.files?.[0]
  target.value = ''
  if (!file) return
  if (!/\.(md|txt)$/i.test(file.name) || file.size > 256 * 1024) {
    ElMessage.warning('请选择小于 256 KB 的 SKILL.md 或文本说明')
    return
  }
  try {
    const content = await file.text()
    const front = content.match(/^\uFEFF?---\r?\n([\s\S]*?)\r?\n---\s*(?:\r?\n|$)/)
    const metadata = (key: string) => {
      const raw = front?.[1].match(new RegExp(`^${key}:\\s*(.+)$`, 'm'))?.[1]?.trim()
      if (!raw) return ''
      try {
        return String(JSON.parse(raw))
      } catch {
        return raw.replace(/^['"]|['"]$/g, '')
      }
    }
    const kind = metadata('kind')
    form.value = {
      ...blankSkillDraft(),
      name: metadata('name') || file.name.replace(/\.(md|txt)$/i, ''),
      description: metadata('description') || '导入的创作技能',
      instructions: front ? content.slice(front[0].length).trim() : content,
      kind: ['method', 'style', 'check', 'tool'].includes(kind) ? (kind as AgentSkill['kind']) : undefined,
      replaces: metadata('replaces') || undefined
    }
    editingBuiltin.value = false
    preservedFiles.value = []
    formVisible.value = true
  } catch (error) {
    console.warn('读取技能说明失败', error)
    ElMessage.error('文件读取失败，请重新选择')
  }
}
function useSkill() {
  if (!selected.value?.enabled) return
  instructionsVisible.value = false
  store.selectedSkillId = selected.value.id
  emit('use')
}
async function remove() {
  if (!selected.value) return
  const id = selected.value.id
  try {
    await inkConfirm('卸载技能及专用依赖环境？文件将移出运行目录并保留恢复副本，已有对话和成果会保留。', '卸载技能', {
      confirmButtonText: '删除',
      cancelButtonText: '取消'
    })
    await store.deleteSkill(id)
    instructionsVisible.value = false
    if (store.selectedSkillId === id) store.selectedSkillId = null
    selectedId.value = filtered.value[0]?.id || ''
  } catch (error) {
    if (error !== 'cancel' && error !== 'close') ElMessage.error(String(error))
  }
}
</script>
