<template>
  <EwModal
    :visible="visible"
    :title="mode === 'install' ? '安装前查看技能' : mode === 'view' ? '技能文件' : '确认脚本'"
    width="760px"
    custom-class="ac-dialog"
    :close-on-click-modal="false"
    manage-focus
    @update:visible="value => emit('update:visible', value)"
  >
    <div v-if="preview" class="ac-skill-review">
      <p
        v-if="mode !== 'view' && preview.files.some(file => file.path.endsWith('.py'))"
        class="ac-index-note warning"
      >
        这些脚本是本地代码，会以你的用户权限在本机运行，与你自己运行脚本的权限相同，能读到本机文件。只安装你信任来源的技能；逐个看过下面的全文再确认。
      </p>
      <dl class="ac-profile-summary">
        <dt>技能</dt>
        <dd>{{ preview.name }}（{{ preview.id }}）</dd>
        <dt>用途</dt>
        <dd>{{ preview.description || '未写用途' }}</dd>
        <dt>依赖</dt>
        <dd>{{ requirementLines.length ? requirementLines.join('、') : '无，只用 Python 标准库' }}</dd>
      </dl>
      <p class="ac-skill-path">{{ preview.directory }}</p>
      <details class="ac-inline-details">
        <summary>
          <span>SKILL.md · 技能说明</span
          ><i class="fa-solid fa-chevron-right ac-details-chevron" aria-hidden="true"></i>
        </summary>
        <pre class="ac-skill-code">{{ preview.instructions }}</pre>
      </details>
      <section v-for="file in preview.files" :key="file.path" class="ac-skill-file">
        <h4>{{ file.path }}</h4>
        <pre v-if="file.content !== null" class="ac-skill-code">{{ file.content }}</pre>
        <p v-else class="ac-muted">资源文件（{{ file.size }} 字节），可以在技能文件夹中查看。</p>
      </section>
      <p v-if="!preview.files.length" class="ac-muted">这个技能没有脚本，只有说明。</p>
    </div>
    <template #footer>
      <button type="button" class="ac-button ink-btn ink-btn-outline" @click="emit('update:visible', false)">
        {{ mode === 'view' ? '关闭' : '取消' }}</button
      ><button
        v-if="mode !== 'view'"
        class="ac-button ink-btn ink-btn-primary"
        type="button"
        :disabled="busy || !preview"
        @click="emit('confirm')"
      >
        {{ mode === 'install' ? '我已看过，安装' : '我已看过，允许运行' }}
      </button>
    </template>
  </EwModal>
</template>

<script setup lang="ts">
import EwModal from '@/components/EwModal/index.vue'
import { computed } from 'vue'
import type { AgentSkillPreview } from '@/types/agent-creation'

const props = defineProps<{
  visible: boolean
  preview: AgentSkillPreview | null
  mode: 'install' | 'approve' | 'view'
  busy?: boolean
}>()
const emit = defineEmits<{ 'update:visible': [value: boolean]; confirm: [] }>()
const requirementLines = computed(() =>
  (props.preview?.requirements || '')
    .split('\n')
    .map(line => line.trim())
    .filter(line => line && !line.startsWith('#'))
)
</script>
