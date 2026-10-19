<template>
  <section v-if="artifact" class="ac-prose" :aria-label="label">
    <header class="ac-prose-head">
      <strong>{{ artifact.title }}</strong>
      <span class="ac-prose-state" :class="{ success: artifact.applied, busy: writing }">{{ stateLabel }}</span>
    </header>
    <div class="ac-prose-body">
      <p v-for="(paragraph, index) in paragraphs" :key="index">{{ paragraph }}</p>
      <p v-if="writing" class="ac-prose-writing" role="status">
        <i class="fa-solid fa-circle-notch fa-spin" aria-hidden="true"></i>正在写…
      </p>
      <p v-else-if="artifact.partial" class="ac-muted" role="status">写作已停止，已生成内容已保留。</p>
      <p v-else-if="!paragraphs.length" class="ac-muted">还没有正文。</p>
    </div>
    <footer v-if="!artifact.partial" class="ac-prose-foot">
      <small>{{ words }} 字</small>
      <div class="ac-actions">
        <button
          v-if="artifact.applied && artifact.kind === 'chapter'"
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          type="button"
          :disabled="busy"
          @click="undo"
        >
          撤销
        </button>
        <button
          v-else-if="artifact.kind === 'chapter' && artifact.bookId"
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          type="button"
          :disabled="busy || !artifact.content.trim()"
          @click="save"
        >
          保存到作品
        </button>
        <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" type="button" @click="store.openedArtifactId = artifact.id">
          编辑
        </button>
      </div>
    </footer>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { useAgentWorkspace } from '../use-agent-workspace'

const props = defineProps<{ artifactId: string }>()
const store = useAgentWorkspace()
const busy = ref(false)
const artifact = computed(() => store.artifacts.find(item => item.id === props.artifactId) || null)
const writing = computed(() => !!artifact.value && store.isArtifactWriting(artifact.value))
const label = computed(() => (artifact.value?.kind === 'short' ? '短篇稿' : artifact.value?.kind === 'draft' ? '试写草稿' : '章节正文'))
// 与宿主同口径：非空行即一段
const paragraphs = computed(() =>
  (artifact.value?.content || '')
    .split(/\n+/)
    .map(item => item.trim())
    .filter(Boolean)
)
const words = computed(() => (artifact.value?.content || '').replace(/\s/g, '').length)
const stateLabel = computed(() => {
  const value = artifact.value
  if (!value) return ''
  if (value.partial) return writing.value ? '正在写…' : '未完成 · 已停止'
  if (value.kind === 'short') return '短篇稿'
  if (value.kind === 'draft') return '试写草稿'
  return value.applied ? '已存为新版本' : '未写入作品'
})
async function save() {
  if (busy.value || !artifact.value) return
  busy.value = true
  try {
    await store.applyArtifact(artifact.value.id)
    ElMessage.success('已保存到作品')
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
async function undo() {
  if (busy.value || !artifact.value) return
  busy.value = true
  try {
    await store.undoArtifact(artifact.value.id)
    ElMessage.success('已撤销，正文回到上一版')
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
</script>
