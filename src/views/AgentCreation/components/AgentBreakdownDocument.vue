<template>
  <section
    v-if="store.openedBreakdown"
    class="ac-document"
    :class="{ 'is-expanded': expanded }"
    aria-label="拆书成果侧栏"
  >
    <header class="ac-document-header">
      <div>
        <h2>《{{ store.openedBreakdown.title }}》拆书结果</h2>
        <p>第 {{ store.openedBreakdown.from }}–{{ store.openedBreakdown.to }} 章 · 拆书报告</p>
      </div>
      <div class="ac-actions">
        <button
          class="ac-icon-button"
          :aria-label="expanded ? '恢复面板宽度' : '展开阅读'"
          @click="$emit('expand')"
        >
          <i :class="expanded ? 'fa-solid fa-compress' : 'fa-solid fa-expand'" aria-hidden="true"></i>
        </button>
        <button class="ac-icon-button" aria-label="关闭拆书面板" @click="store.openedBreakdown = null">
          <i class="fa-solid fa-xmark" aria-hidden="true"></i>
        </button>
      </div>
    </header>
    <div class="breakdown-document-body">
      <BreakdownResultsContent
        :key="store.openedBreakdown.projectId"
        :project-id="store.openedBreakdown.projectId"
        :session-id="store.activeId || undefined"
        :from="store.openedBreakdown.from"
        :to="store.openedBreakdown.to"
        :revision="job?.revision"
        @close="store.openedBreakdown = null"
      />
    </div>
  </section>
</template>
<script setup lang="ts">
import { computed } from 'vue'
import BreakdownResultsContent from '@/components/BreakdownResultsContent.vue'
import { useAgentWorkspace } from '../use-agent-workspace'
defineProps<{ expanded?: boolean }>()
defineEmits<{ expand: [] }>()
const store = useAgentWorkspace()
const job = computed(() => store.breakdownJobs.find(j => j.projectId === store.openedBreakdown?.projectId))
</script>
<style scoped>
.breakdown-document-body {
  overflow: auto;
  min-height: 0;
  flex: 1;
  padding: 20px 24px;
}
</style>
