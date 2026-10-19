<template>
  <section class="ac-batch ac-breakdown ac-distillation" :aria-label="`蒸馏任务：${job.title}`">
    <header class="ac-batch-head">
      <div class="ac-distillation-title">
        <strong>蒸馏 · 《{{ job.title }}》</strong
        ><span class="ac-batch-status" :class="job.status">{{ status }}</span>
      </div>
      <div v-if="job.status !== 'awaiting_scope'" class="ac-actions">
        <button
          v-if="job.status === 'running'"
          class="ac-button ink-btn ink-btn-outline"
          :disabled="busy"
          @click="control('pause')"
        >
          暂停
        </button>
        <button
          v-if="job.status === 'paused' || job.status === 'failed'"
          class="ac-button ink-btn ink-btn-primary"
          :disabled="busy"
          @click="control('resume')"
        >
          继续
        </button>
        <button
          v-if="['running', 'paused', 'failed'].includes(job.status)"
          class="ac-button ink-btn ink-btn-outline"
          :disabled="busy"
          @click="control('cancel')"
        >
          取消
        </button>
      </div>
    </header>
    <div v-if="job.status === 'awaiting_scope'" class="ac-distillation-scope">
      <p class="ac-batch-summary">
        共 {{ job.totalChapters }} 章，约 {{ formatChars(job.totalChars) }} 字。请选择本次蒸馏范围。
      </p>
      <div class="ac-distillation-options">
        <el-radio-group v-model="mode" aria-label="蒸馏范围" :disabled="busy">
          <el-radio-button label="full">全书拆解蒸馏</el-radio-button>
          <el-radio-button label="range">指定章节范围</el-radio-button>
        </el-radio-group>
        <div v-if="mode === 'range'" class="ac-distillation-fields">
          <div class="ac-actions ac-distillation-range">
            <span>第</span
            ><el-input-number
              v-model="from"
              :min="1"
              :max="job.totalChapters"
              :precision="0"
              controls-position="right"
              aria-label="起始章节"
              :disabled="busy"
            />
            <span>至</span
            ><el-input-number
              v-model="to"
              :min="from || 1"
              :max="job.totalChapters"
              :precision="0"
              controls-position="right"
              aria-label="结束章节"
              :disabled="busy"
            /><span>章</span>
          </div>
          <p class="ac-muted ac-breakdown-tip">章节范围按导入目录顺序计算。</p>
        </div>
      </div>
      <p class="ac-batch-summary">
        本次 {{ selectedCount }} 章，约
        {{ formatChars(selectedChars) }} 字。逐章分析、分层归纳和原文回查均使用平台配置的分析模型；实际 token
        用量随进度记录。
      </p>
      <button class="ac-button ink-btn ink-btn-primary" :disabled="busy || !validRange" @click="start">
        {{ busy ? '正在准备…' : '确认范围并开始' }}
      </button>
    </div>
    <div v-else class="ac-distillation-progress">
      <p class="ac-batch-summary">第 {{ job.scope?.from }}–{{ job.scope?.to }} 章 · {{ phase }}</p>
      <div
        class="ac-breakdown-bar"
        role="progressbar"
        :aria-valuenow="job.doneChapters"
        :aria-valuemin="0"
        :aria-valuemax="job.selectedChapters"
        aria-label="原文阅读进度"
      >
        <span
          :style="{ width: `${job.selectedChapters ? (job.doneChapters / job.selectedChapters) * 100 : 0}%` }"
        ></span>
      </div>
      <p class="ac-batch-summary">
        已读 {{ job.doneChapters }}/{{ job.selectedChapters }} 章 · 已完成 {{ job.doneUnits }}/{{
          job.totalUnits
        }}
        个分析步骤<template v-if="job.failedUnits && job.status === 'running'">
          · {{ job.failedUnits }} 个片段失败，其余跑完后可继续</template
        ><template v-if="job.usage.total_tokens">
          · {{ formatTokens(job.usage.total_tokens) }} tokens</template
        >
      </p>
      <p v-if="job.error" class="ac-breakdown-error" role="alert">{{ job.error }}</p>
      <p v-else-if="job.status === 'paused'" class="ac-muted ac-breakdown-tip">
        已暂停领取新任务；正在处理的片段完成后保存，继续时复用已完成结果。
      </p>
      <p v-else-if="job.status === 'completed'" class="ac-muted ac-breakdown-tip">
        已完成所选范围的阅读、归纳与核验；写作时按场景取范例原文给写作模型，效果以试写为准。
      </p>
    </div>
  </section>
</template>
<script setup lang="ts">
import { computed, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { agentCommand } from '@/agent/client'
import { formatTokens } from '@/agent/reference-book'
import type { AgentDistillationJob } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
const props = defineProps<{ job: AgentDistillationJob }>()
const store = useAgentWorkspace()
const mode = ref<'full' | 'range'>('full')
const from = ref<number | undefined>(1)
const to = ref<number | undefined>(props.job.totalChapters)
const busy = ref(false)
const status = computed(
  () =>
    ({
      awaiting_scope: '选择范围',
      running: '进行中',
      paused: '已暂停',
      failed: '待重试',
      cancelled: '已取消',
      completed: '已完成'
    })[props.job.status]
)
const phase = computed(
  () =>
    ({
      reading: '逐段读取原文',
      summarizing: '对着范例归纳写法卡',
      verifying: '对照范例核验写法卡',
      compiling: '整理文风档案'
    })[props.job.phase]
)
const validRange = computed(
  () =>
    mode.value === 'full' ||
    (typeof from.value === 'number' &&
      typeof to.value === 'number' &&
      from.value >= 1 &&
      to.value >= from.value &&
      to.value <= props.job.totalChapters)
)
const selectedCount = computed(() =>
  mode.value === 'full' ? props.job.totalChapters : validRange.value ? to.value! - from.value! + 1 : 0
)
const selectedChars = computed(() =>
  mode.value === 'full'
    ? props.job.totalChars
    : validRange.value
      ? (props.job.chapterSizes || []).slice(from.value! - 1, to.value).reduce((a, b) => a + b, 0)
      : 0
)
const formatChars = (n: number) => (n >= 10000 ? `${(n / 10000).toFixed(1)} 万` : n.toLocaleString())
async function invoke(action: string, args: object) {
  busy.value = true
  try {
    store.upsertDistillation(await agentCommand<AgentDistillationJob>(action, { id: props.job.id, ...args }))
  } catch (e) {
    ElMessage.error(String(e))
  } finally {
    busy.value = false
  }
}
const start = () => invoke('distillation_start', { mode: mode.value, from: from.value, to: to.value })
const control = (control: string) => invoke('distillation_control', { control })
</script>
<style scoped>
.ac-distillation {
  display: grid;
  gap: 8px;
  padding: 24px;
}
.ac-distillation-title {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px 12px;
}
.ac-distillation-title .ac-batch-status {
  margin-left: 0;
  white-space: nowrap;
}
.ac-distillation p {
  margin: 0;
  line-height: 1.65;
}
.ac-distillation-scope,
.ac-distillation-options,
.ac-distillation-fields,
.ac-distillation-progress {
  display: grid;
  min-width: 0;
}
.ac-distillation-scope {
  gap: 20px;
}
.ac-distillation-scope > .ac-button {
  justify-self: start;
}
.ac-distillation-options {
  gap: 16px;
}
.ac-distillation-fields {
  gap: 8px;
}
.ac-distillation-range {
  flex-wrap: wrap;
  gap: 12px;
}
.ac-distillation-range .el-input-number {
  width: 112px;
}
.ac-distillation-progress {
  gap: 12px;
}
.ac-distillation .ac-breakdown-bar {
  margin: 0;
}
@media (max-width: 640px) {
  .ac-distillation {
    padding: 20px 16px;
  }
  .ac-distillation-range {
    gap: 8px;
  }
}
</style>
