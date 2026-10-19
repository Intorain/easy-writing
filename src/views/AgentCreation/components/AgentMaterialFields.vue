<template>
  <fieldset class="ac-material-fields" :disabled="readonly">
    <label v-for="field in visibleFields" :key="field.key">
      <span>{{ field.label }}</span>
      <select
        v-if="field.options"
        class="ink-input"
        :value="fields[field.key]"
        @change="setField(field.key, Number(($event.target as HTMLSelectElement).value))"
      >
        <option v-for="(label, value) in field.options" :key="value" :value="value">{{ label }}</option>
      </select>
      <input
        v-else-if="['name', 'title', 'age', 'parentId', 'tags', 'aliases'].includes(field.key)"
        class="ink-input"
        :value="
          Array.isArray(fields[field.key]) ? (fields[field.key] as string[]).join('，') : fields[field.key]
        "
        @input="setText(field.key, ($event.target as HTMLInputElement).value)"
      />
      <textarea
        v-else
        class="ink-input"
        :value="fields[field.key]"
        rows="5"
        @input="setText(field.key, ($event.target as HTMLTextAreaElement).value)"
      ></textarea>
    </label>
  </fieldset>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { AgentArtifact } from '@/types/agent-creation'
const props = defineProps<{ fields: NonNullable<AgentArtifact['materialFields']>; readonly: boolean }>()
const emit = defineEmits<{ change: [fields: NonNullable<AgentArtifact['materialFields']>] }>()
const fieldDefinitions: Array<{ key: string; label: string; options?: string[] }> = [
  { key: 'name', label: '名称' },
  { key: 'title', label: '标题' },
  { key: 'role', label: '角色定位', options: ['主角', '主要配角', '反派', '路人'] },
  { key: 'gender', label: '性别', options: ['女', '男', '其他'] },
  { key: 'age', label: '年龄' },
  { key: 'tags', label: '标签（逗号分隔）' },
  { key: 'aliases', label: '别名（逗号分隔）' },
  { key: 'appearance', label: '外貌' },
  { key: 'personality', label: '性格' },
  { key: 'background', label: '背景' },
  { key: 'ability', label: '能力' },
  { key: 'voice', label: '语言指纹' },
  {
    key: 'type',
    label: '设定类型',
    options: ['地理位置', '势力宗门', '功法技能', '物品道具', '等级体系', '其他']
  },
  { key: 'nodeType', label: '大纲类型', options: ['目录', '内容'] },
  { key: 'content', label: '大纲内容' },
  { key: 'detail', label: '设定内容' }
]
// 仅编辑候选提交的字段；未涉及的正式字段由保存服务保留。
const visibleFields = computed(() => fieldDefinitions.filter(field => field.key in props.fields))
function setField(key: string, value: string | number | string[]) {
  emit('change', { ...props.fields, [key]: value })
}
function setText(key: string, value: string) {
  setField(
    key,
    key === 'tags' || key === 'aliases'
      ? value
          .split(/[,，]/)
          .map(s => s.trim())
          .filter(Boolean)
      : value
  )
}
</script>
