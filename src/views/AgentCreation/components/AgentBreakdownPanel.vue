<template>
  <section
    v-for="job in [jobValue]"
    :key="job.id"
    class="ac-batch ac-breakdown"
    :aria-label="`拆书进度：${job.title}`"
  >
    <header class="ac-batch-head">
      <div>
        <strong>拆书 · 《{{ job.title }}》第 {{ job.from }}–{{ job.to }} 章</strong>
        <span class="ac-batch-status" :class="job.status">{{ statusLabel(job) }}</span>
      </div>
      <div class="ac-actions">
        <button
          class="ac-button ink-btn ink-btn-outline"
          @click="
            store.openBreakdown({
              projectId: job.projectId,
              title: job.title,
              from: job.from,
              to: job.to,
              createdAt: job.createdAt || 0,
              status: job.status
            })
          "
        >
          查看结果
        </button>
        <button
          v-if="job.status === 'running'"
          class="ac-button ink-btn ink-btn-outline"
          type="button"
          :disabled="busy === job.id"
          @click="control(job, 'pause')"
        >
          暂停
        </button>
        <button
          v-if="
            job.status === 'paused' || job.status === 'failed' || (job.status === 'completed' && job.failed)
          "
          class="ac-button ink-btn ink-btn-primary"
          type="button"
          :disabled="busy === job.id"
          @click="control(job, 'resume')"
        >
          {{ job.status === 'completed' ? `重试失败的 ${job.failed} 章` : '继续' }}
        </button>
        <button
          v-if="job.status !== 'completed'"
          class="ac-button ink-btn ink-btn-outline"
          type="button"
          :disabled="busy === job.id"
          @click="control(job, 'cancel')"
        >
          取消
        </button>
        <template v-else>
          <button
            v-if="!job.continuedAt"
            class="ac-button ink-btn ink-btn-primary"
            type="button"
            @click="continueAnalysis(job)"
          >
            让 Agent 继续分析
          </button>
          <button
            class="ac-button ink-btn ink-btn-outline"
            type="button"
            @click="store.dismissBreakdown(job)"
          >
            收起
          </button>
        </template>
      </div>
    </header>
    <div
      class="ac-breakdown-bar"
      role="progressbar"
      :aria-valuenow="job.done"
      :aria-valuemin="0"
      :aria-valuemax="job.total"
      :aria-label="`已拆 ${job.done} / ${job.total} 章`"
    >
      <span :style="{ width: `${percent(job)}%` }"></span>
    </div>
    <p class="ac-batch-summary">
      已拆 {{ job.done }}/{{ job.total }} 章
      <template v-if="job.usage?.total_tokens">
        · 已用约 {{ formatTokens(job.usage.total_tokens) }} token</template
      >
      <template v-if="job.status === 'running' && left(job) > 0 && job.remainingMinutes">
        · 预计还需 {{ formatMinutes(job.remainingMinutes) }}</template
      >
      <template v-if="job.failed"> · {{ job.failed }} 章提取失败</template>
    </p>
    <p v-if="job.status === 'failed' && job.error" class="ac-breakdown-error" role="alert">{{ job.error }}</p>
    <p v-else-if="job.status === 'running'" class="ac-muted ac-breakdown-tip">
      在后台逐章进行，可以照常聊天和写作。
      <template v-if="job.autoContinue"
        >拆完后 Agent 会自动接着分析<button
          class="ac-text-link"
          type="button"
          :disabled="busy === job.id"
          @click="control(job, 'auto_off')"
        >
          不用自动接着
        </button></template
      >
    </p>
    <p v-else-if="job.status === 'completed' && job.continuedAt" class="ac-muted ac-breakdown-tip">
      已拆完，Agent 已自动接着分析，结果在对话里。
    </p>
    <p v-else-if="job.status === 'completed' && job.continueSkipped" class="ac-muted ac-breakdown-tip">
      已拆完；{{ job.continueSkipped }}，没有自动接着分析，可以点“让 Agent 继续分析”。
    </p>
  </section>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { ElMessage } from 'element-plus'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentBreakdownJob } from '@/types/agent-creation'
import { formatMinutes, formatTokens } from '@/agent/reference-book'
import { useAgentWorkspace } from '../use-agent-workspace'

defineProps<{ jobValue: AgentBreakdownJob }>()
const emit = defineEmits<{ continue: [] }>()
const store = useAgentWorkspace()
const busy = ref<string | null>(null)

const left = (job: AgentBreakdownJob) => Math.max(0, job.total - job.done - job.failed)
const percent = (job: AgentBreakdownJob) => (job.total ? Math.round((job.done / job.total) * 100) : 0)
const statusLabel = (job: AgentBreakdownJob) =>
  ({
    running: '拆解中',
    paused: '已暂停',
    failed: '已停下',
    completed: job.failed ? '已结束' : '已拆完',
    cancelled: '已取消'
  })[job.status]

async function control(job: AgentBreakdownJob, action: 'pause' | 'resume' | 'cancel' | 'auto_off') {
  try {
    if (action === 'cancel') {
      await inkConfirm(
        `还没拆的 ${left(job)} 章不再拆，已拆的 ${job.done} 章保留，之后可以重新发起。`,
        '取消拆书',
        {
          confirmButtonText: '取消拆书',
          cancelButtonText: '继续拆',
          type: 'warning'
        }
      )
    }
    busy.value = job.id
    await store.controlBreakdown(job.projectId, action)
  } catch (error) {
    if (error === 'cancel' || error === 'close') return
    ElMessage.error(String(error instanceof Error ? error.message : error))
  } finally {
    busy.value = null
  }
}

/** 拆完后交回作者：在输入框里放好一句“继续”，由作者决定发不发、补不补要求。 */
function continueAnalysis(job: AgentBreakdownJob) {
  store.draft = `《${job.title}》第 ${job.from}–${job.to} 章已经拆完，请继续刚才的分析。`
  emit('continue')
}
</script>
