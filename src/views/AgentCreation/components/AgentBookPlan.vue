<template>
  <fieldset class="ac-material-fields ac-book-plan" :disabled="readonly">
    <p class="ac-muted">按八步完善开书方案。需要你决定的内容可以选择、修改，也可以交给 AI 拟定。</p>
    <section v-for="step in steps" :key="step.key" class="ac-plan-step" :class="`is-${step.status}`" :aria-current="progress?.current === step.key ? 'step' : undefined">
      <header>
        <strong>{{ step.number }} {{ step.label }}</strong>
        <span class="ac-plan-state">{{ stateText(step) }}</span>
      </header>

      <template v-if="step.key === 'scope'">
        <label v-for="d in step.decisions" :key="d.key"
          ><span>{{ d.label }}<em v-if="decisionBy(d.key) && decisionBy(d.key) !== 'author'">{{ decisionBy(d.key) === 'assumed' ? '按委托拟定' : 'AI 建议' }}</em></span
          ><el-input
            class="ink-select"
            :model-value="decisionValue(d.key)"
            :disabled="readonly"
            :aria-label="d.label"
            maxlength="120"
            :placeholder="d.placeholder"
            @update:model-value="changeDecision(d.key, $event)"
        /></label>
        <label
          ><span>读者群</span
          ><el-input
            class="ink-select"
            :model-value="plan.audience || ''"
            :disabled="readonly"
            aria-label="读者群"
            placeholder="例如：男频、女频或不限"
            @update:model-value="change('audience', $event)"
        /></label>
      </template>

      <template v-else-if="step.key === 'protagonist'">
        <template v-if="!plan.characters?.length">
        <label v-for="d in step.decisions" :key="d.key"
          ><span>{{ d.label }}<em v-if="decisionBy(d.key) && decisionBy(d.key) !== 'author'">{{ decisionBy(d.key) === 'assumed' ? '按委托拟定' : 'AI 建议' }}</em></span
          ><el-input
            class="ink-select"
            :model-value="decisionValue(d.key)"
            :disabled="readonly"
            :aria-label="d.label"
            maxlength="120"
            :placeholder="d.placeholder"
            @update:model-value="changeDecision(d.key, $event)"
        /></label>
        </template>
        <div v-for="(character, index) in plan.characters || []" :key="index" class="ac-plan-character">
          <header>
            <strong>{{ character.name || '未命名' }}<small>{{ character.role }}</small></strong>
            <button type="button" class="ac-text-link" :disabled="readonly" @click="removeCharacter(index)">删掉</button>
          </header>
          <label v-for="field in characterFields" :key="field.key"
            ><span>{{ field.label }}</span
            ><el-select
              v-if="field.key === 'role'"
              class="ink-select"
              popper-class="ink-select-popper"
              :model-value="character.role"
              :disabled="readonly"
              :aria-label="`${character.name || '人物'}的身份`"
              @update:model-value="changeCharacter(index, 'role', $event)"
            >
              <el-option v-for="role in ['主角', '配角', '反派']" :key="role" :value="role" :label="role" />
            </el-select><el-input
              v-else
              class="ink-select"
              :type="field.textarea ? 'textarea' : 'text'"
              :autosize="field.textarea ? { minRows: 2, maxRows: 8 } : undefined"
              :model-value="character[field.key] || ''"
              :disabled="readonly"
              :aria-label="`${character.name || '人物'}的${field.label}`"
              @update:model-value="changeCharacter(index, field.key, $event)"
          /></label>
        </div>
        <button type="button" class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="readonly" @click="addCharacter">
          添加人物
        </button>
        <label
          ><span>人物概述</span
          ><el-input
            class="ac-book-plan-textarea"
            type="textarea"
            :autosize="{ minRows: 2, maxRows: 12 }"
            resize="none"
            :model-value="plan.protagonist || ''"
            :disabled="readonly"
            aria-label="人物概述"
            placeholder="主角的身份、处境及重要人物关系"
            @update:model-value="change('protagonist', $event)"
        /></label>
      </template>

      <template v-else-if="step.key === 'world'">
        <div v-for="(setting, index) in plan.world || []" :key="index" class="ac-plan-character">
          <header>
            <strong>{{ setting.name || '未命名' }}</strong>
            <button type="button" class="ac-text-link" :disabled="readonly" @click="removeSetting(index)">删掉</button>
          </header>
          <label
            ><span>名称</span
            ><el-input class="ink-select" :model-value="setting.name" :disabled="readonly" aria-label="设定名称" @update:model-value="changeSetting(index, 'name', $event)"
          /></label>
          <label
            ><span>内容</span
            ><el-input
              class="ink-select"
              type="textarea"
              :autosize="{ minRows: 2, maxRows: 8 }"
              :model-value="setting.detail"
              :disabled="readonly"
              aria-label="设定内容"
              @update:model-value="changeSetting(index, 'detail', $event)"
          /></label>
        </div>
        <button type="button" class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="readonly" @click="addSetting">
          添加设定
        </button>
      </template>

      <template v-else-if="step.key === 'packaging'">
        <label
          ><span>书名</span
          ><el-input
            class="ink-select"
            :model-value="plan.workingTitle"
            :disabled="readonly"
            aria-label="书名"
            maxlength="120"
            placeholder="在备选里挑一个，或者自己写"
            @update:model-value="change('workingTitle', $event)"
        /></label>
        <label
          ><span>简介</span
          ><el-input
            class="ac-book-plan-textarea"
            type="textarea"
            :autosize="{ minRows: 3, maxRows: 16 }"
            resize="none"
            :model-value="plan.intro || ''"
            :disabled="readonly"
            aria-label="简介"
            placeholder="读者在书城看到的那段话"
            @update:model-value="change('intro', $event)"
        /></label>
      </template>

      <template v-else-if="step.key === 'build'">
        <p class="ac-muted">{{ buildText }}</p>
      </template>

      <template v-else>
        <p v-if="step.key === 'hook' && selectedStory" class="ac-muted">已沿用你选择的故事方案：{{ selectedStory.card.genre }}</p>
        <label v-for="d in step.decisions" :key="d.key"
          ><span>{{ d.label }}<em v-if="decisionBy(d.key) && decisionBy(d.key) !== 'author'">{{ decisionBy(d.key) === 'assumed' ? '按委托拟定' : 'AI 建议' }}</em></span
          ><el-input
            class="ink-select"
            :model-value="decisionValue(d.key)"
            :disabled="readonly"
            :aria-label="d.label"
            maxlength="120"
            :placeholder="d.placeholder"
            @update:model-value="changeDecision(d.key, $event)"
        /></label>
        <label v-for="field in step.fields" :key="field.key"
          ><span>{{ field.label }}</span
          ><el-input
            class="ac-book-plan-textarea"
            type="textarea"
            :autosize="{ minRows: 3, maxRows: 24 }"
            resize="none"
            :model-value="plan[field.key] || ''"
            :disabled="readonly"
            :aria-label="field.label"
            :placeholder="field.placeholder"
            @update:model-value="change(field.key, $event)"
          />
        </label>
      </template>
    </section>

    <details class="ac-plan-preferences">
      <summary>写作偏好</summary>
      <p class="ac-muted">叙述人称在人物设定阶段确认；已有作品沿用原设置。</p>
      <p v-if="plan.writingPreferences?.legacyNarrationNote" class="ac-muted">原有叙述说明：{{ plan.writingPreferences.legacyNarrationNote }}</p>
      <p v-if="plan.styleSuggestions" class="ac-muted">AI 文风建议（可调整）：{{ plan.styleSuggestions }}</p>
      <label><span>叙述人称<em v-if="preferenceBy('narration') && preferenceBy('narration') !== 'author'">{{ preferenceBy('narration') === 'assumed' ? '按委托拟定' : 'AI 建议' }}</em></span>
        <el-select class="ink-select" popper-class="ink-select-popper" :model-value="preferenceValue('narration')" :disabled="readonly" aria-label="叙述人称" placeholder="沿用已有设置" @update:model-value="changePreference('narration', $event)">
          <el-option value="第三人称" label="第三人称" /><el-option value="第一人称" label="第一人称" />
        </el-select>
      </label>
      <label><span>叙述视角</span><el-input class="ink-select" :model-value="preferenceValue('viewpoint')" :disabled="readonly" aria-label="叙述视角" placeholder="例如：主要跟随主角，只呈现他知道的信息" @update:model-value="changePreference('viewpoint', $event)" /></label>
      <label><span>故事基调</span><el-input class="ink-select" :model-value="preferenceValue('tone')" :disabled="readonly" aria-label="故事基调" placeholder="例如：轻松、严肃或黑暗，与题材分开说明" @update:model-value="changePreference('tone', $event)" /></label>
        <label
          ><span>平台写作设置</span>
          <el-select
            class="ink-select"
            popper-class="ink-select-popper"
            :model-value="plan.profileId"
            :disabled="readonly"
            aria-label="平台写作设置"
            placeholder="还没定时使用通用设置"
            @update:model-value="change('profileId', $event)"
          >
            <el-option v-for="profile in profiles" :key="profile.id" :value="profile.id" :label="profile.name" />
          </el-select>
        </label>
        <label
          ><span>文风要求</span
          ><el-input
            class="ink-select"
            type="textarea"
            :model-value="plan.styleRequirements || ''"
            :disabled="readonly"
            aria-label="文风要求"
            placeholder="你已经明确的表达习惯，写的时候照着来"
            @update:model-value="change('styleRequirements', $event)"
        /></label>
        <label
          ><span>规划方式</span
          ><el-select
            class="ink-select"
            popper-class="ink-select-popper"
            :model-value="plan.writingMode || 'explore'"
            :disabled="readonly"
            aria-label="规划方式"
            @update:model-value="change('writingMode', $event)"
          >
            <el-option value="explore" label="先试写，再完善规划" />
            <el-option value="outline" label="先理顺主线，再写正文" /> </el-select
        ></label>
    </details>

    <label
      ><span>未定事项</span
      ><el-input
        class="ac-book-plan-textarea"
        type="textarea"
        :autosize="{ minRows: 2, maxRows: 12 }"
        resize="none"
        :model-value="plan.unresolved?.join('\n') || ''"
        :disabled="readonly"
        aria-label="未定事项"
        placeholder="一行一项"
        @update:model-value="
          change(
            'unresolved',
            $event.split('\n').filter((s: string) => s.trim())
          )
        "
      />
    </label>
  </fieldset>
</template>
<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { agentCommand } from '@/agent/client'
import type { AgentArtifact, AgentBookPlan, AgentBookPlanProgress } from '@/types/agent-creation'
import type { PlanCharacter, PlanDecisions, PlanSetting, PlanWritingPreferences } from '@/types/canon'
const profiles = ref<Array<{ id: string; name: string }>>([])
onMounted(async () => {
  try {
    profiles.value = await agentCommand('writing_profiles')
  } catch (error) {
    ElMessage.error(`读取写作方向失败：${String(error)}`)
  }
})
const props = defineProps<{ plan: AgentBookPlan; progress?: AgentBookPlanProgress; selectedStory?: AgentArtifact['selectedDirection']; readonly: boolean }>()
const emit = defineEmits<{ change: [plan: AgentBookPlan] }>()

type DecisionKey = keyof PlanDecisions
type TextField = 'premise' | 'sellingPoint' | 'conflict' | 'outline' | 'opening'
interface StepSpec {
  key: string
  label: string
  decisions: Array<{ key: DecisionKey; label: string; placeholder: string }>
  fields: Array<{ key: TextField; label: string; placeholder: string }>
}
/** 与宿主 agent_book_plan::STEPS 同一张表：步骤顺序与每步作者拍板的事 */
const STEPS: StepSpec[] = [
  { key: 'scope', label: '题材定位', decisions: [
    { key: 'genre', label: '题材', placeholder: '例如：都市高武，偏悬疑调查' },
    { key: 'platform', label: '发布平台', placeholder: '例如：番茄' }
  ], fields: [] },
  { key: 'hook', label: '故事构思', decisions: [
    { key: 'hook', label: '已选故事方向', placeholder: '你选定或委托拟定的故事构思' }
  ], fields: [
    { key: 'premise', label: '故事构思', placeholder: '主角身份、核心设定、起始事件与主要阻力组成的完整故事' },
    { key: 'sellingPoint', label: '核心卖点', placeholder: '这份故事方案区别于同类作品的吸引点' },
    { key: 'conflict', label: '主要冲突', placeholder: '谁或什么阻碍主角达成目标' }
  ] },
  { key: 'protagonist', label: '人物设定', decisions: [
    { key: 'protagonistName', label: '主角姓名', placeholder: '选择姓名或交给 AI 拟定' },
    { key: 'protagonistGoal', label: '行动目标', placeholder: '主角在故事前期要完成的具体事情' }
  ], fields: [] },
  { key: 'world', label: '世界观设定', decisions: [], fields: [] },
  { key: 'story', label: '剧情大纲', decisions: [], fields: [
    { key: 'outline', label: '剧情大纲', placeholder: '主线、阶段发展和关键事件，不要求提前定下全书结局' }
  ] },
  { key: 'opening', label: '开篇章纲', decisions: [
    { key: 'openingDirection', label: '已选开篇方案', placeholder: '你选定或委托拟定的开篇事件' }
  ], fields: [
    { key: 'opening', label: '前三章章纲', placeholder: '前三章分别发生什么、人物如何选择、事件怎样衔接' }
  ] },
  { key: 'packaging', label: '书名与简介', decisions: [], fields: [] },
  { key: 'build', label: '创建作品', decisions: [], fields: [] }
]
const characterFields: Array<{ key: keyof PlanCharacter; label: string; textarea?: boolean }> = [
  { key: 'name', label: '姓名' },
  { key: 'role', label: '身份（主角 / 配角 / 反派）' },
  { key: 'summary', label: '身份与处境', textarea: true },
  { key: 'goal', label: '行动目标' },
  { key: 'motivation', label: '人物动机', textarea: true },
  { key: 'longTermGoal', label: '长期目标' },
  { key: 'personality', label: '性格与缺点', textarea: true },
  { key: 'ability', label: '特殊能力与限制（如有）', textarea: true }
]
const status = (key: string) => props.progress?.steps.find(step => step.key === key)?.status || 'todo'
const steps = computed(() => STEPS.map((step, index) => ({ ...step, number: index + 1, status: status(step.key) })))
const STATE_TEXT: Record<string, string> = { done: '已确定', proposed: '已拟定', assumed: '按委托拟定', partial: '部分已拟定', todo: '待完善' }
function stateText(step: { key: string; status: string }) {
  if (step.key === 'build') return step.status === 'done' ? '已创建' : props.progress?.buildable ? '可以创建' : '尚未完成'
  return STATE_TEXT[step.status] || ''
}
const buildText = computed(() => {
  if (props.progress?.steps.find(step => step.key === 'build')?.status === 'done') return '作品已经建好，书名随时可改。'
  const blocking = props.progress?.blocking || []
  if (!props.progress) return '正在保存方案…'
  return blocking.length ? `创建作品还需确定或委托拟定：${blocking.join('、')}。` : '开书所需的决定已完成，可以创建作品。'
})
const decisionValue = (key: DecisionKey) => props.plan.decisions?.[key]?.value || ''
const decisionBy = (key: DecisionKey) => props.plan.decisions?.[key]?.by
const preferenceValue = (key: 'narration' | 'viewpoint' | 'tone') => props.plan.writingPreferences?.[key]?.value || (key === 'narration' ? props.plan.decisions?.narration?.value : '') || ''
const preferenceBy = (key: 'narration' | 'viewpoint' | 'tone') => props.plan.writingPreferences?.[key]?.by || (key === 'narration' ? props.plan.decisions?.narration?.by : undefined)
function changePreference(key: 'narration' | 'viewpoint' | 'tone', value: string) {
  const writingPreferences: PlanWritingPreferences = { ...(props.plan.writingPreferences || {}) }
  if (key === 'narration') {
    if (value !== '第一人称' && value !== '第三人称') return
    writingPreferences.narration = { value, by: 'author' }
  } else writingPreferences[key] = { value, by: 'author' }
  change('writingPreferences', writingPreferences)
}
function change(key: string, value: unknown) {
  emit('change', { ...props.plan, [key]: value })
}
/** 作者在面板里改的，就是作者定的 */
function changeDecision(key: DecisionKey, value: string) {
  const decisions: PlanDecisions = { ...(props.plan.decisions || {}) }
  if (value.trim()) decisions[key] = { value, by: 'author' }
  else delete decisions[key]
  const field = key === 'protagonistName' ? 'name' : key === 'protagonistGoal' ? 'goal' : undefined
  const characters = props.plan.characters?.map(c => c.role === '主角' && field ? { ...c, [field]: value } : c)
  emit('change', { ...props.plan, decisions, ...(characters ? { characters } : {}) })
}
function changeCharacter(index: number, key: keyof PlanCharacter, value: string) {
  const characters = (props.plan.characters || []).map((c, i) => (i === index ? { ...c, [key]: value } : c))
  const decision = key === 'name' ? 'protagonistName' : key === 'goal' ? 'protagonistGoal' : undefined
  if (characters[index]?.role === '主角' && decision) {
    const decisions: PlanDecisions = { ...(props.plan.decisions || {}) }
    if (value.trim()) decisions[decision] = { value, by: 'author' }
    else delete decisions[decision]
    emit('change', { ...props.plan, characters, decisions })
  } else change('characters', characters)
}
function addCharacter() {
  change('characters', [...(props.plan.characters || []), { name: '', role: '配角', summary: '' } as PlanCharacter])
}
function removeCharacter(index: number) {
  change('characters', (props.plan.characters || []).filter((_, i) => i !== index))
}
function changeSetting(index: number, key: 'name' | 'detail', value: string) {
  change('world', (props.plan.world || []).map((w, i) => (i === index ? { ...w, [key]: value } : w)))
}
function addSetting() {
  change('world', [...(props.plan.world || []), { name: '', detail: '' } as PlanSetting])
}
function removeSetting(index: number) {
  change('world', (props.plan.world || []).filter((_, i) => i !== index))
}
</script>

<style scoped lang="scss">
.ac-book-plan-textarea {
  --el-input-bg-color: var(--input-bg);
  --el-input-text-color: var(--ink-main);
  --el-input-border-color: var(--input-border);
  --el-input-hover-border-color: var(--input-focus-border);
  --el-input-focus-border-color: var(--input-focus-border);
  --el-input-placeholder-color: var(--ink-sec);

  :deep(.el-textarea__inner) {
    line-height: 1.7;
    padding: 10px 12px;
  }
}
.ac-plan-preferences {
  padding: 16px 0;
  summary { cursor: pointer; font-weight: 600; }
}
.ac-plan-step {
  border-top: 1px solid var(--input-border);
  padding-top: 12px;
  margin-top: 4px;

  > header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }
  &.is-done .ac-plan-state {
    color: var(--ink-success, #3a7d44);
  }
  &.is-assumed .ac-plan-state {
    color: var(--ink-accent, #8a5a2b);
  }
}
.ac-plan-state {
  font-size: 12px;
  color: var(--ink-sec);
  white-space: nowrap;
}
label span em {
  font-style: normal;
  font-size: 12px;
  color: var(--ink-accent, #8a5a2b);
  margin-left: 6px;
}
.ac-plan-character {
  border: 1px dashed var(--input-border);
  border-radius: 8px;
  padding: 10px 12px;
  margin-bottom: 8px;

  > header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 6px;

    small {
      margin-left: 6px;
      color: var(--ink-sec);
      font-weight: normal;
    }
  }
}
</style>
