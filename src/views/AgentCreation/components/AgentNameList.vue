<template>
  <div class="ac-names">
    <p class="ac-name-check-summary">
      {{ checkSummary || '这些标记来自候选及本地资料核对，不代表已通过全网重名、平台审核或市场效果验证。' }}
    </p>
    <ol class="ac-name-list" aria-label="取名候选">
      <li v-for="item in candidates" :key="item.name" :class="{ flagged: item.flags.length }">
        <div>
          <strong>{{ item.name }}</strong>
          <span v-if="item.meaning" class="ac-muted">{{ item.meaning }}</span>
        </div>
        <dl v-if="item.readerPromise || item.storyBasis || item.tradeoff" class="ac-name-rationale">
          <template v-if="item.readerPromise"
            ><dt>读者期待</dt>
            <dd>{{ item.readerPromise }}</dd></template
          >
          <template v-if="item.storyBasis"
            ><dt>设定依据</dt>
            <dd>{{ item.storyBasis }}</dd></template
          >
          <template v-if="item.tradeoff"
            ><dt>命名取舍</dt>
            <dd>{{ item.tradeoff }}</dd></template
          >
        </dl>
        <p v-for="flag in item.flags" :key="flag" class="ac-name-flag">
          <i class="fa-solid fa-triangle-exclamation" aria-hidden="true"></i>{{ flag }}
        </p>
      </li>
    </ol>
  </div>
</template>

<script setup lang="ts">
import type { AgentArtifact } from '@/types/agent-creation'

defineProps<{ candidates: NonNullable<AgentArtifact['candidates']>; checkSummary?: string }>()
</script>

<style scoped>
.ac-names {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.ac-name-check-summary {
  margin: 20px 24px 0;
  color: var(--ac-muted);
  font-size: 0.8125rem;
  line-height: 1.7;
}
.ac-name-rationale {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 6px 12px;
  margin-top: 12px;
  font-size: 0.875rem;
  line-height: 1.7;
}
.ac-name-rationale dt {
  color: var(--ac-muted);
}
.ac-name-rationale dd {
  margin: 0;
  overflow-wrap: anywhere;
}
</style>
