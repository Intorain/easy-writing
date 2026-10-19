<template>
  <EwModal
    :visible="visible"
    title="拆书结果"
    width="880px"
    manage-focus
    @update:visible="emit('update:visible', $event)"
  >
    <BreakdownResultsContent
      v-if="visible && projectId"
      :project-id="projectId"
      @close="emit('update:visible', false)"
    />
  </EwModal>
</template>
<script setup lang="ts">
import EwModal from './EwModal/index.vue'
import BreakdownResultsContent from './BreakdownResultsContent.vue'
defineProps<{ visible: boolean; projectId: string }>()
const emit = defineEmits<{ 'update:visible': [value: boolean] }>()
</script>
