<template>
  <section v-if="job" class="ac-batch" :aria-label="isShort ? '批量短篇进度' : '批量连载进度'">
    <header class="ac-batch-head">
      <div>
        <strong>{{ isShort ? '批量' : '批量连载 · ' }}{{ job.bookTitle || '当前作品' }}</strong>
        <span class="ac-batch-status" :class="job.status">{{ statusLabel }}</span>
      </div>
      <div class="ac-actions">
        <button
          v-if="['queued', 'running', 'waiting'].includes(job.status)"
          class="ac-button ink-btn ink-btn-outline"
          type="button"
          @click="control('pause')"
        >
          暂停
        </button>
        <button
          v-if="['waiting', 'paused', 'failed'].includes(job.status)"
          class="ac-button ink-btn ink-btn-primary"
          type="button"
          :disabled="job.waitingReason === 'question'"
          :title="job.waitingReason === 'question' ? '先在对话里回答问题' : ''"
          @click="control('resume')"
        >
          继续
        </button>
        <button class="ac-button ink-btn ink-btn-outline" type="button" @click="control('cancel')">
          取消
        </button>
      </div>
    </header>
    <p class="ac-batch-summary">
      {{ done }}/{{ job.queue.length }} {{ unit }}完成 ·
      {{ isShort ? '只出稿子' : job.mode === 'write' ? '直接写入' : '只出稿子，不写入' }} ·
      {{ policyLabel }}
      <span v-if="job.usage?.total_tokens">· {{ job.usage.total_tokens }} tokens</span>
    </p>
    <ol class="ac-batch-list">
      <li v-for="entry in job.queue" :key="entry.index" :class="entry.status">
        <span class="ac-batch-dot" aria-hidden="true"></span>
        <span class="ac-batch-title">{{ entry.title }}</span>
        <span class="ac-batch-flags">
          <span v-if="entry.blocked" class="block">阻断</span>
          <span v-else-if="entry.suspects" class="suspect">疑点 {{ entry.suspects }}</span>
          <span v-if="entry.applied" class="applied">已写入</span>
          <span v-if="entry.error" class="error" :title="entry.error">失败</span>
        </span>
        <button
          v-if="entry.artifactId"
          class="ac-text-link"
          type="button"
          @click="store.openedArtifactId = entry.artifactId"
        >
          查看稿子
        </button>
      </li>
    </ol>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import { useAgentWorkspace } from '../use-agent-workspace'

const store = useAgentWorkspace()
const job = computed(() => store.activeJob)
const done = computed(() => job.value?.queue.filter(entry => entry.status === 'done').length || 0)
const isShort = computed(() => job.value?.mode === 'short')
const unit = computed(() => (isShort.value ? '篇' : '章'))
const statusLabel = computed(() => {
  const value = job.value
  if (!value) return ''
  if (value.status === 'waiting') {
    return (
      {
        question: '等待你回答问题',
        review: `本${unit.value}完成，等待你审阅`,
        issue: '本步尚未完成，继续处理当前稿件',
        error: '出错，等待处理'
      }[value.waitingReason || 'review'] || '等待中'
    )
  }
  return {
    queued: '排队中',
    running: '正在写',
    paused: '已暂停',
    completed: '已完成',
    cancelled: '已取消',
    failed: '失败'
  }[value.status]
})
const policyLabel = computed(
  () =>
    ({ each: `每${unit.value}停`, every5: `每五${unit.value}停`, onIssue: '有疑点才停' })[
      job.value?.reviewPolicy || 'onIssue'
    ]
)
async function control(action: 'pause' | 'resume' | 'cancel') {
  if (!job.value) return
  try {
    if (action === 'cancel') {
      await inkConfirm('取消后未完成的章节不再生成，已写出的稿子保留。', '取消批量连载', {
        confirmButtonText: '取消任务',
        cancelButtonText: '继续保留',
        type: 'warning'
      })
    }
    await store.controlBatch(job.value.id, action)
  } catch (error) {
    if (error === 'cancel' || error === 'close') return
    ElMessage.error(String(error))
  }
}
</script>
