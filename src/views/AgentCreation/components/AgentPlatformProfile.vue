<template>
  <EwModal
    :visible="visible"
    title="编辑平台画像"
    width="640px"
    custom-class="ac-dialog"
    :close-on-click-modal="false"
    manage-focus
    @update:visible="value => emit('update:visible', value)"
  >
    <form id="agent-profile-form" class="ac-form ac-profile-form" @submit.prevent="save">
      <label>画像名称<input v-model="form.name" class="ink-input" required maxlength="60" /></label>
      <div class="ac-form-grid">
        <label
          >单章字数下限<input
            v-model.number="form.minWords"
            class="ink-input"
            type="number"
            min="100"
            step="100"
        /></label>
        <label
          >单章字数上限<input
            v-model.number="form.maxWords"
            class="ink-input"
            type="number"
            min="100"
            step="100"
        /></label>
        <label
          >段落最长字数<input
            v-model.number="form.paragraphMaxChars"
            class="ink-input"
            type="number"
            min="10"
        /></label>
        <label
          >开篇多少字内要有冲突<input
            v-model.number="form.openingHookWithin"
            class="ink-input"
            type="number"
            min="0"
        /></label>
        <label
          >每几章至少一个小爽点<input
            v-model.number="form.payoffEvery"
            class="ink-input"
            type="number"
            min="1"
        /></label>
        <label
          >每几章一个中等爽点<input
            v-model.number="form.midPayoffEvery"
            class="ink-input"
            type="number"
            min="1"
        /></label>
        <label
          >压抑最多连续几章<input
            v-model.number="form.suppressMaxChapters"
            class="ink-input"
            type="number"
            min="0"
        /></label>
        <label for="agent-profile-info"
          >设定释放
          <el-select
            id="agent-profile-info"
            v-model="form.infoReleaseLevel"
            class="ink-select"
            popper-class="ink-select-popper"
            aria-label="设定释放"
          >
            <el-option value="low" label="少：用到才解释" />
            <el-option value="mid" label="中" />
            <el-option value="high" label="多：可以集中铺设定" />
          </el-select>
        </label>
      </div>
      <el-checkbox v-model="form.endHookRequired">每章结尾必须留钩子</el-checkbox>
      <label
        >读者画像<textarea
          v-model="form.readerPersona"
          class="ink-input"
          rows="3"
          maxlength="2000"
          placeholder="读者模拟评审按这个身份打分，如：手机碎片阅读，三段没冲突就划走"
        ></textarea>
      </label>
      <fieldset class="ac-gate-editor">
        <legend>体检各项的级别</legend>
        <p class="ac-muted ac-form-note">
          阻断会拦下保存；疑点附原文等你裁决；提示只供参考。空稿、字数不到下限一半、整段重复、敏感词固定为阻断。
        </p>
        <div v-for="item in GATE_CODES" :key="item.code" class="ac-gate-row">
          <span>{{ item.label }}</span>
          <el-select
            v-model="form.gate[item.code]"
            class="ink-select"
            popper-class="ink-select-popper"
            :aria-label="`${item.label}的级别`"
            :disabled="item.locked"
          >
            <el-option value="block" label="阻断" />
            <el-option value="suspect" label="疑点" />
            <el-option value="notice" label="提示" />
          </el-select>
        </div>
      </fieldset>
    </form>
    <template #footer>
      <button type="button" class="ac-button ink-btn ink-btn-outline" @click="emit('update:visible', false)">
        取消</button
      ><button
        class="ac-button ink-btn ink-btn-primary"
        type="submit"
        form="agent-profile-form"
        :disabled="saving"
      >
        保存画像
      </button>
    </template>
  </EwModal>
</template>

<script setup lang="ts">
import EwModal from '@/components/EwModal/index.vue'
import { reactive, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import type { AgentPlatformProfile as Profile } from '@/types/agent-creation'
import type { PlatformProfile } from '@/types/canon'
import { useAgentWorkspace } from '../use-agent-workspace'

type Level = 'block' | 'suspect' | 'notice'
/** 质量门会产出的检查项；结构类固定为阻断，保证不会存下空稿或整段重复 */
const GATE_CODES: Array<{ code: string; label: string; locked?: boolean }> = [
  { code: 'structure.empty', label: '空稿', locked: true },
  { code: 'structure.words_min', label: '字数不到下限一半', locked: true },
  { code: 'structure.repeat', label: '整段重复', locked: true },
  { code: 'structure.sensitive', label: '自定义敏感词', locked: true },
  { code: 'overlap.reference', label: '与参考书原文逐字相同', locked: true },
  { code: 'structure.words_low', label: '字数低于下限' },
  { code: 'structure.words_high', label: '字数远超上限' },
  { code: 'state.alive', label: '已死亡人物再出场' },
  { code: 'state.conflict', label: '同一时间点状态矛盾' },
  { code: 'style.repeat_head', label: '相邻段落开头相同' }
]
/** 与宿主 agent_quality::default_level 一致：画像没写的检查项用它 */
const defaultLevel = (code: string): Level =>
  (code.startsWith('structure.') && !['structure.words_low', 'structure.words_high'].includes(code)) || code === 'overlap.reference'
    ? 'block'
    : code.startsWith('state.')
      ? 'suspect'
      : 'notice'

const props = defineProps<{ visible: boolean; profile: Profile | null }>()
const emit = defineEmits<{ 'update:visible': [value: boolean]; saved: [profile: Profile] }>()
const store = useAgentWorkspace()
const saving = ref(false)
const form = reactive({
  id: '',
  name: '',
  minWords: 2000,
  maxWords: 4000,
  paragraphMaxChars: 100,
  openingHookWithin: 500,
  payoffEvery: 3,
  midPayoffEvery: 8,
  suppressMaxChapters: 4,
  infoReleaseLevel: 'mid' as PlatformProfile['infoReleaseLevel'],
  endHookRequired: false,
  readerPersona: '',
  gate: {} as Record<string, Level>,
  /** 画像里有、但这里没列出的检查项，保存时原样保留 */
  extraGate: {} as Record<string, Level>
})

function levelOf(profile: Profile, code: string): Level | undefined {
  return (['block', 'suspect', 'notice'] as Level[]).find(level => profile.gate?.[level]?.includes(code))
}
watch(
  () => [props.visible, props.profile] as const,
  ([visible, profile]) => {
    if (!visible || !profile) return
    form.id = profile.id
    form.name = profile.name
    form.minWords = profile.chapterWords[0]
    form.maxWords = profile.chapterWords[1]
    form.paragraphMaxChars = profile.paragraphMaxChars
    form.openingHookWithin = profile.openingHookWithin
    form.payoffEvery = profile.payoffEvery
    form.midPayoffEvery = profile.midPayoffEvery
    form.suppressMaxChapters = profile.suppressMaxChapters
    form.infoReleaseLevel = profile.infoReleaseLevel
    form.endHookRequired = profile.endHookRequired
    form.readerPersona = profile.readerPersona
    const listed = new Set(GATE_CODES.map(item => item.code))
    form.gate = Object.fromEntries(
      GATE_CODES.map(item => [
        item.code,
        item.locked ? 'block' : levelOf(profile, item.code) || defaultLevel(item.code)
      ])
    )
    form.extraGate = {}
    for (const level of ['block', 'suspect', 'notice'] as Level[])
      for (const code of profile.gate?.[level] || []) if (!listed.has(code)) form.extraGate[code] = level
  },
  { immediate: true }
)

const count = (value: number, min = 0) => Math.max(min, Math.round(Number(value) || 0))
async function save() {
  const name = form.name.trim()
  const minWords = count(form.minWords, 1)
  const maxWords = count(form.maxWords, 1)
  if (!name) return
  if (minWords > maxWords) {
    ElMessage.warning('字数下限不能大于上限')
    return
  }
  const gate: PlatformProfile['gate'] = { block: [], suspect: [], notice: [] }
  for (const [code, level] of Object.entries({ ...form.extraGate, ...form.gate })) gate[level].push(code)
  const profile: PlatformProfile = {
    id: form.id,
    name,
    chapterWords: [minWords, maxWords],
    paragraphMaxChars: count(form.paragraphMaxChars, 1),
    endHookRequired: form.endHookRequired,
    openingHookWithin: count(form.openingHookWithin),
    payoffEvery: count(form.payoffEvery, 1),
    midPayoffEvery: count(form.midPayoffEvery, 1),
    suppressMaxChapters: count(form.suppressMaxChapters),
    infoReleaseLevel: form.infoReleaseLevel,
    readerPersona: form.readerPersona.trim(),
    gate
  }
  saving.value = true
  try {
    const saved = await store.savePlatformProfile(profile)
    emit('saved', saved)
    emit('update:visible', false)
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    saving.value = false
  }
}
</script>
