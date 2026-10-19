<template>
  <EwModal
    :visible="visible"
    :title="copy ? '编辑技能副本' : draft.id ? '编辑技能' : '新建技能'"
    width="900px"
    custom-class="ac-dialog ac-skill-editor"
    :close-on-click-modal="false"
    manage-focus
    @update:visible="close"
  >
    <p v-if="copy" class="ac-index-note">
      保存为自定义副本，原内置技能保留。副本中的脚本需重新确认后才能运行。
    </p>
    <form id="agent-skill-form" @submit.prevent="save">
      <AgentSkillFields v-model="draft" :readonly="saving" :preserved-files="preservedFiles" />
      <p v-if="error" class="ac-skill-error" role="alert">{{ error }}</p>
    </form>
    <template #footer>
      <button type="button" class="ac-button ink-btn ink-btn-outline" :disabled="saving" @click="close">
        取消
      </button>
      <button
        type="submit"
        form="agent-skill-form"
        class="ac-button ink-btn ink-btn-primary"
        :disabled="saving"
      >
        {{ saving ? '正在保存…' : copy ? '保存自定义副本' : '保存技能' }}
      </button>
    </template>
  </EwModal>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import EwModal from '@/components/EwModal/index.vue'
import AgentSkillFields from './AgentSkillFields.vue'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentSkill, AgentSkillDraft } from '@/types/agent-creation'
import { blankSkillDraft } from '@/agent/skill-draft'
import { useAgentWorkspace } from '../use-agent-workspace'

const props = withDefaults(
  defineProps<{
    visible: boolean
    initial: AgentSkillDraft
    copy?: boolean
    preservedFiles?: string[]
  }>(),
  { preservedFiles: () => [] }
)
const emit = defineEmits<{ 'update:visible': [value: boolean]; saved: [value: AgentSkill] }>()
const store = useAgentWorkspace()
const draft = ref<AgentSkillDraft>(blankSkillDraft())
const baseline = ref('')
const saving = ref(false)
const error = ref('')
watch(
  () => props.visible,
  visible => {
    if (!visible) return
    draft.value = JSON.parse(JSON.stringify(props.initial))
    baseline.value = JSON.stringify(draft.value)
    error.value = ''
  },
  { immediate: true }
)
async function close() {
  if (saving.value) return
  if (JSON.stringify(draft.value) !== baseline.value) {
    try {
      await inkConfirm('关闭会放弃本次尚未保存的修改。', '放弃修改？', {
        confirmButtonText: '放弃修改',
        cancelButtonText: '继续编辑'
      })
    } catch {
      return
    }
  }
  emit('update:visible', false)
}
async function save() {
  if (saving.value) return
  saving.value = true
  error.value = ''
  try {
    const payload = {
      ...draft.value,
      kind: draft.value.kind || null,
      replaces: draft.value.kind === 'method' ? draft.value.replaces || null : null
    }
    const saved = await store.saveSkill(payload)
    emit('saved', saved)
    emit('update:visible', false)
    ElMessage.success(saved.python ? '技能已保存，请查看并确认脚本后运行' : '技能已保存')
  } catch (cause) {
    error.value = String(cause)
  } finally {
    saving.value = false
  }
}
</script>
