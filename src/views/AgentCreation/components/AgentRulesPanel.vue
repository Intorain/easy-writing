<template>
  <section class="ac-canon" aria-label="规则与画像">
    <p v-if="loadError" class="ac-index-note warning" role="alert">{{ loadError }}</p>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>作者规则</h3>
          <p>写作、规划与体检时必须遵守。禁忌类规则里用引号括起来的词会作为检查线索；具体是否需要修改仍按你的要求与语境判断。</p>
        </div>
        <button class="ac-button ink-btn ink-btn-primary" @click="openRuleForm">
          <i class="fa-solid fa-plus" aria-hidden="true"></i>添加规则
        </button>
      </header>
      <ul v-if="rules.length" class="ac-rule-list">
        <li v-for="rule in rules" :key="rule.id">
          <span class="ac-source-tag">{{ rule.category }}</span>
          <div>
            <p>{{ rule.text }}</p>
            <small>{{ rule.scope === 'author' ? '所有作品' : '本书' }} · {{ rule.source }}</small>
          </div>
          <button
            class="ac-icon-button"
            :aria-label="`删除规则：${rule.text}`"
            :disabled="busy"
            @click="removeRule(rule)"
          >
            <i class="fa-regular fa-trash-can" aria-hidden="true"></i>
          </button>
        </li>
      </ul>
      <p v-else class="ac-small-empty">还没有规则。比如“每章结尾留钩子”“主角不杀无辜”“不写系统提示音”。</p>
    </div>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>平台画像</h3>
          <p>
            决定单章字数、段落长度、钩子与爽点频率，以及体检各项是阻断、疑点还是提示。数值是可改的默认参数，不是定论。
          </p>
        </div>
        <button class="ac-button ink-btn ink-btn-outline" :disabled="!currentProfile" @click="editProfile">
          <i class="fa-solid fa-pen" aria-hidden="true"></i
          >{{ currentProfile?.builtin ? '复制并修改' : '编辑画像' }}
        </button>
      </header>
      <div class="ac-canon-field ac-select-wrap">
        <el-select
          :model-value="profileId"
          class="ink-select"
          popper-class="ink-select-popper"
          aria-label="本书使用的平台画像"
          :disabled="busy"
          @update:model-value="changeProfile"
        >
          <el-option
            v-for="item in profiles"
            :key="item.id"
            :label="item.builtin ? item.name : `${item.name}（我的）`"
            :value="item.id"
          />
        </el-select>
      </div>
      <dl v-if="currentProfile" class="ac-profile-summary">
        <dt>单章字数</dt>
        <dd>{{ currentProfile.chapterWords[0] }}–{{ currentProfile.chapterWords[1] }} 字</dd>
        <dt>段落长度</dt>
        <dd>不超过 {{ currentProfile.paragraphMaxChars }} 字</dd>
        <dt>开篇</dt>
        <dd>{{ currentProfile.openingHookWithin }} 字内出现冲突或悬念</dd>
        <dt>章末钩子</dt>
        <dd>{{ currentProfile.endHookRequired ? '每章必须' : '建议' }}</dd>
        <dt>爽点频率</dt>
        <dd>
          每 {{ currentProfile.payoffEvery }} 章至少一个小爽点，每
          {{ currentProfile.midPayoffEvery }} 章一个中等爽点
        </dd>
        <dt>压抑上限</dt>
        <dd>不超过 {{ currentProfile.suppressMaxChapters }} 章</dd>
        <dt>读者画像</dt>
        <dd>{{ currentProfile.readerPersona }}</dd>
      </dl>
    </div>

    <div class="ac-canon-section">
      <header class="ac-canon-section-head">
        <div>
          <h3>规则集预览</h3>
          <p>
            Agent 写这本书时按“作者本轮原话 &gt; 作者规则 &gt; 写作方法技能 &gt;
            平台画像”合成一份规则集，规划、起草、体检共用同一份。
          </p>
        </div>
      </header>
      <div class="ac-canon-field ac-select-wrap">
        <el-select
          v-model="previewSkill"
          class="ink-select"
          popper-class="ink-select-popper"
          aria-label="写作方法技能"
          placeholder="自动选择写作方法"
        >
          <el-option label="自动选择写作方法" value="" />
          <el-option v-for="skill in methodSkills" :key="skill.id" :label="skill.name" :value="skill.id" />
        </el-select>
      </div>
      <p v-if="previewLoading" class="ac-muted">正在合成…</p>
      <template v-else-if="preview">
        <div v-if="'toolError' in preview" class="ac-index-note warning">
          {{ previewErrorText(preview.toolError) }}
          <span v-if="preview.toolError.candidates?.length">
            候选：{{ preview.toolError.candidates.map(item => item.name).join('、') }}。可在上方指定一个，或到
            Skills 页停用其余的。
          </span>
        </div>
        <template v-else>
          <ol class="ac-ruleset-layers">
            <li>
              <strong>作者规则</strong
              ><span>{{ preview.rules.length ? `${preview.rules.length} 条` : '无' }}</span>
            </li>
            <li>
              <strong>写作方法</strong
              ><span>{{ preview.skillName || '未启用写作方法技能，按模型默认写法' }}</span>
            </li>
            <li>
              <strong>平台画像</strong><span>{{ preview.profileName }}</span>
            </li>
          </ol>
          <details class="ac-inline-details ac-ruleset-full">
            <summary>
              <span>模型看到的完整规则文本</span
              ><i class="fa-solid fa-chevron-right ac-details-chevron" aria-hidden="true"></i>
            </summary>
            <pre>{{ preview.expanded }}</pre>
          </details>
          <p class="ac-muted ac-form-note">
            规则指纹
            {{ preview.fingerprint }}：规则、画像或技能变化后指纹随之改变，已开始的写作任务会提示规则已过期。
          </p>
        </template>
      </template>
    </div>

    <EwModal
      v-model:visible="ruleFormVisible"
      title="添加规则"
      width="520px"
      custom-class="ac-dialog"
      :close-on-click-modal="false"
      manage-focus
    >
      <form id="agent-rule-form" class="ac-form" @submit.prevent="saveRule">
        <label
          >规则内容<textarea
            v-model="ruleForm.text"
            class="ink-input"
            required
            maxlength="600"
            rows="3"
            placeholder="写一条明确的要求，如：每章结尾留一个具体的钩子"
          ></textarea>
        </label>
        <div class="ac-form-grid">
          <label for="agent-rule-category"
            >类别
            <el-select
              id="agent-rule-category"
              v-model="ruleForm.category"
              class="ink-select"
              popper-class="ink-select-popper"
              aria-label="规则类别"
            >
              <el-option v-for="item in categories" :key="item" :label="item" :value="item" />
            </el-select>
          </label>
          <label for="agent-rule-scope"
            >生效范围
            <el-select
              id="agent-rule-scope"
              v-model="ruleForm.scope"
              class="ink-select"
              popper-class="ink-select-popper"
              aria-label="生效范围"
            >
              <el-option value="book" label="只对本书" />
              <el-option value="author" label="所有作品" />
            </el-select>
          </label>
        </div>
      </form>
      <template #footer>
        <button type="button" class="ac-button ink-btn ink-btn-outline" @click="ruleFormVisible = false">
          取消</button
        ><button
          class="ac-button ink-btn ink-btn-primary"
          type="submit"
          form="agent-rule-form"
          :disabled="busy"
        >
          保存规则
        </button>
      </template>
    </EwModal>
    <AgentMethodPack @imported="load" />
    <AgentPlatformProfile
      v-model:visible="profileFormVisible"
      :profile="editingProfile"
      @saved="onProfileSaved"
    />
  </section>
</template>

<script setup lang="ts">
import EwModal from '@/components/EwModal/index.vue'
import { computed, reactive, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type {
  AgentPlatformProfile as Profile,
  AgentRule,
  AgentRuleCategory,
  AgentRulesetPreviewResult
} from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
import AgentPlatformProfile from './AgentPlatformProfile.vue'
import AgentMethodPack from './AgentMethodPack.vue'

const props = defineProps<{ bookId: string }>()
const store = useAgentWorkspace()
const categories: AgentRuleCategory[] = ['结构', '文风', '禁忌', '人设', '平台']
const busy = ref(false)
const loadError = ref('')
const rules = ref<AgentRule[]>([])
const profiles = ref<Profile[]>([])
const profileId = ref('')
const currentProfile = computed(() => profiles.value.find(item => item.id === profileId.value))
const methodSkills = computed(() => store.skills.filter(skill => skill.enabled && skill.kind === 'method'))

async function load() {
  const bookId = props.bookId
  if (!bookId) return
  try {
    const [nextRules, nextProfiles] = await Promise.all([
      store.canonQuery<AgentRule[]>('rules', bookId),
      store.canonQuery<{ current: string; profiles: Profile[] }>('profiles', bookId)
    ])
    if (bookId !== props.bookId) return
    // 只列全局规则与本书规则；宿主已按作品过滤
    rules.value = nextRules
    profiles.value = nextProfiles.profiles
    profileId.value = nextProfiles.current
    loadError.value = ''
  } catch (error) {
    loadError.value = `读取规则失败：${String(error)}`
  }
  await refreshPreview()
}

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

// ---- 作者规则 ----
const ruleFormVisible = ref(false)
const ruleForm = reactive<{ text: string; category: AgentRuleCategory; scope: AgentRule['scope'] }>({
  text: '',
  category: '结构',
  scope: 'book'
})
function openRuleForm() {
  ruleForm.text = ''
  ruleForm.category = '结构'
  ruleForm.scope = 'book'
  ruleFormVisible.value = true
}
function saveRule() {
  const text = ruleForm.text.trim()
  if (!text) return
  return run(async () => {
    await store.saveRule({ scope: ruleForm.scope, bookId: props.bookId, category: ruleForm.category, text })
    ruleFormVisible.value = false
    ElMessage.success('规则已保存，之后的写作与体检会使用它')
    await load()
  })
}
function removeRule(rule: AgentRule) {
  return run(async () => {
    await inkConfirm(
      rule.scope === 'author' ? '删除这条规则？它对所有作品生效，删除后都不再使用。' : '删除这条规则？',
      '删除规则',
      { confirmButtonText: '删除', cancelButtonText: '取消', type: 'warning' }
    )
    await store.deleteRule(rule.id)
    await load()
  })
}

// ---- 平台画像 ----
const profileFormVisible = ref(false)
const editingProfile = ref<Profile | null>(null)
function changeProfile(value: string) {
  if (!value || value === profileId.value) return
  return run(async () => {
    await store.setBookProfile(props.bookId, value)
    profileId.value = value
    ElMessage.success('已切换平台画像，之后的写作与体检按新画像执行')
    await refreshPreview()
  })
}
function editProfile() {
  const profile = currentProfile.value
  if (!profile) return
  // 内置画像不直接改：复制一份作者副本，保存后本书改用副本
  const copy = JSON.parse(JSON.stringify(profile)) as Profile
  editingProfile.value = profile.builtin
    ? { ...copy, id: `mine-${Date.now().toString(36)}`, name: `${profile.name}（我的）`, builtin: false }
    : copy
  profileFormVisible.value = true
}
function onProfileSaved(profile: Profile) {
  return run(async () => {
    if (profile.id !== profileId.value) await store.setBookProfile(props.bookId, profile.id)
    ElMessage.success('画像已保存')
    await load()
  })
}

// ---- 规则集预览 ----
const previewSkill = ref('')
const preview = ref<AgentRulesetPreviewResult | null>(null)
const previewLoading = ref(false)
async function refreshPreview() {
  const bookId = props.bookId
  if (!bookId) return
  previewLoading.value = true
  try {
    const result = await store.rulesetPreview(bookId, previewSkill.value)
    if (bookId === props.bookId) preview.value = result
  } catch (error) {
    preview.value = { ok: false, toolError: { code: 'error', message: String(error) } }
  } finally {
    previewLoading.value = false
  }
}
function previewErrorText(error: { code: string; message: string }) {
  if (error.code === 'ruleset_conflict')
    return '有多个写作方法技能同时启用且替换同一目标，Agent 写作前会先请你选一个。'
  if (error.code === 'skill_not_method') return '选定的技能不是已启用的写作方法技能。'
  return error.message
}
watch(previewSkill, () => void refreshPreview())
watch(
  () => props.bookId,
  () => {
    preview.value = null
    void load()
  },
  { immediate: true }
)
</script>
