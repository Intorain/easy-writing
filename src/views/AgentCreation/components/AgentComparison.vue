<template>
  <section class="ac-comparison-plan" aria-label="仿书对照方案">
    <p class="ac-muted">
      对标书只借方法与结构；“我的做法”来自你自己的人物、设定与冲突。要改哪一行，直接在对话里告诉 Agent。
    </p>
    <div class="ac-comparison-table" role="table" aria-label="对照方案">
      <div class="ac-comparison-row is-head" role="row">
        <span role="columnheader">维度</span><span role="columnheader">对标做法</span
        ><span role="columnheader">我的做法</span><span role="columnheader">差异说明</span>
      </div>
      <div v-for="(row, index) in rows" :key="index" class="ac-comparison-row" role="row">
        <strong role="cell">{{ row.dimension }}</strong
        ><span role="cell">{{ row.benchmark }}</span
        ><span role="cell">{{ row.mine }}</span
        ><span role="cell">{{ row.difference }}</span>
      </div>
    </div>
    <div class="ac-actions">
      <button class="ac-button ink-btn ink-btn-outline" type="button" @click="$emit('use', 'plan')">
        据此准备开书</button
      ><button class="ac-button ink-btn ink-btn-outline" type="button" @click="$emit('use', 'outline')">
        据此规划章纲
      </button>
    </div>
  </section>
</template>

<script setup lang="ts">
import type { AgentComparisonRow } from '@/types/agent-creation'

defineProps<{ rows: AgentComparisonRow[] }>()
defineEmits<{ use: [target: 'plan' | 'outline'] }>()
</script>
