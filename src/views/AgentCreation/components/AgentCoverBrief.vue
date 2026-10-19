<template>
  <div class="ac-cover-brief">
    <div class="ac-cover-brief-main">
      <label class="ac-cover-brief-field"
        >画面描述
        <textarea
          :value="artifact.content"
          rows="8"
          spellcheck="false"
          :readonly="readonly"
          @input="$emit('change', ($event.target as HTMLTextAreaElement).value)"
        ></textarea>
      </label>
      <p class="ac-muted">
        风格：{{ artifact.coverStyle || '自动匹配' }}。点“生成封面”在封面工具里按这段描述出图、排书名，确认后保存到作品。
      </p>
      <div class="ac-actions">
        <button class="ac-button ink-btn ink-btn-primary" type="button" :disabled="!artifact.content.trim()" @click="open = true">
          <i class="fa-solid fa-image" aria-hidden="true"></i>{{ artifact.coverUrl ? '重新生成' : '生成封面' }}
        </button>
      </div>
    </div>
    <figure v-if="artifact.coverUrl" class="ac-cover-brief-preview">
      <img :src="artifact.coverUrl" alt="已生成的封面" />
      <figcaption>已生成的封面</figcaption>
    </figure>
    <AICreateCover
      v-model:visible="open"
      :book-id="bookNumber"
      :book-title="artifact.bookTitle"
      :initial-prompt="artifact.content"
      :initial-style="artifact.coverStyle"
      @apply="onApply"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { ElMessage } from 'element-plus'
import AICreateCover from '@/views/Home/components/AICreateCover.vue'
import type { AgentArtifact } from '@/types/agent-creation'
import { agentCommand } from '@/agent/client'

const props = defineProps<{ artifact: AgentArtifact; readonly?: boolean }>()
const emit = defineEmits<{ change: [value: string]; covered: [artifact: AgentArtifact] }>()
const open = ref(false)
/** 本地书编号是负数；没关联作品时封面工具里自己选书 */
const bookNumber = computed(() => {
  const value = Number(props.artifact.bookId)
  return Number.isFinite(value) && value !== 0 ? value : undefined
})

async function onApply(payload: { coverUrl: string }) {
  open.value = false
  if (!payload.coverUrl) return
  try {
    emit('covered', await agentCommand<AgentArtifact>('attach_cover', { id: props.artifact.id, coverUrl: payload.coverUrl }))
  } catch (error) {
    ElMessage.error(String(error))
  }
}
</script>
