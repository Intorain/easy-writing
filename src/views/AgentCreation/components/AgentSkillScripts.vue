<template>
  <div class="ac-detail-section ac-skill-scripts">
    <strong>技能文件</strong>
    <p v-if="loading" class="ac-muted">正在检查文件与运行环境…</p>
    <p v-if="error" role="alert" class="ac-skill-error">{{ error }}</p>
    <template v-if="status">
      <p class="ac-skill-state" role="status">{{ stateLabel }}</p>
      <p v-if="status.error" class="ac-skill-error">{{ status.error }}</p>
      <p v-if="status.missingScripts.length" class="ac-skill-error">
        说明引用的脚本尚未导入：{{ status.missingScripts.join('、') }}
      </p>
      <dl v-if="status.scripts.length">
        <dt>脚本</dt>
        <dd>{{ status.scripts.join('、') }}</dd>
        <dt>Python</dt>
        <dd>
          {{
            status.runtime?.available
              ? `${status.runtime.source === 'portable' ? '便携版' : '系统'} ${status.runtime.version}`
              : '未找到可用的 Python'
          }}
        </dd>
        <dt>依赖</dt>
        <dd>
          {{
            status.requirements.length
              ? `${status.depsReady ? '已安装' : '未安装'}：${status.requirements.join('、')}`
              : '无第三方依赖'
          }}
        </dd>
        <dt>运行确认</dt>
        <dd>
          {{
            skill.fileSource === 'builtin'
              ? '内置脚本，随应用分发'
              : status.approved
                ? '已确认当前版本'
                : '待确认当前版本'
          }}
        </dd>
      </dl>
      <p v-if="status.lastRun" class="ac-form-note" :class="{ 'ac-skill-error': !status.lastRun.ok }">
        最近运行 {{ status.lastRun.script }}：{{
          status.lastRun.ok ? '成功' : status.lastRun.error || '运行失败'
        }}
        {{ status.lastRun.scriptHash !== status.scriptHash ? '（旧版本）' : '' }}
      </p>
      <p class="ac-skill-path">{{ status.directory || skill.filePath }}</p>
      <div class="ac-actions ac-skill-script-actions">
        <button
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          :disabled="busy"
          @click="openReview('view')"
        >
          查看文件
        </button>
        <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy" @click="emit('edit')">
          {{ skill.fileSource === 'builtin' ? '编辑副本' : '编辑文件' }}
        </button>
        <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" :disabled="busy" @click="openFolder">
          打开文件夹
        </button>
        <button class="ac-button ink-btn ink-btn-outline ink-btn-sm" @click="copyPath">复制路径</button>
        <button
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          :disabled="busy || loading"
          @click="refresh"
        >
          刷新状态
        </button>
        <button
          v-if="status.scripts.length && !status.approved"
          class="ac-button ink-btn ink-btn-primary ink-btn-sm"
          :disabled="busy"
          @click="openReview('approve')"
        >
          查看并确认运行
        </button>
        <button
          v-if="status.scripts.length && !status.runtime?.available && status.runtime?.portableDownloadable"
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          :disabled="busy"
          @click="installRuntime"
        >
          下载便携 Python
        </button>
        <button
          v-if="status.runtime?.available && status.requirements.length && !status.depsReady"
          class="ac-button ink-btn ink-btn-outline ink-btn-sm"
          :disabled="busy || !status.approved"
          @click="installDeps"
        >
          安装依赖
        </button>
      </div>
      <p v-if="status.scripts.length" class="ac-muted ac-form-note">本地代码，与你自己运行的脚本权限相同。</p>
      <p v-if="busy" class="ac-muted">{{ busyText }}</p>
    </template>
    <AgentSkillReview
      v-model:visible="reviewVisible"
      :preview="preview"
      :mode="reviewMode"
      :busy="busy"
      @confirm="approve"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { agentCommand } from '@/agent/client'
import { inkConfirm } from '@/utils/ink-confirm'
import type { AgentSkill, AgentSkillPreview, AgentSkillStatus } from '@/types/agent-creation'
import { useAgentWorkspace } from '../use-agent-workspace'
import AgentSkillReview from './AgentSkillReview.vue'

const props = defineProps<{ skill: AgentSkill }>()
const emit = defineEmits<{ edit: [] }>()
const store = useAgentWorkspace()
const error = ref('')
const reviewMode = ref<'view' | 'approve'>('view')
const status = ref<AgentSkillStatus | null>(null)
const loading = ref(false)
const busy = ref(false)
const busyText = ref('')
const reviewVisible = ref(false)
const preview = ref<AgentSkillPreview | null>(null)

const stateLabel = computed(() =>
  status.value
    ? {
        instructions_only: '纯说明技能',
        missing_files: '缺少文件',
        missing_runtime: '缺少 Python 环境',
        missing_dependencies: '缺少依赖',
        pending_confirmation: '等待运行确认',
        ready: '可运行',
        failed: '上次运行失败',
        disabled: '技能已停用'
      }[status.value.state]
    : ''
)
async function load() {
  loading.value = true
  error.value = ''
  try {
    status.value = await agentCommand<AgentSkillStatus>('skill_status', { id: props.skill.id })
  } catch (cause) {
    error.value = String(cause)
  } finally {
    loading.value = false
  }
}
async function run(text: string, task: () => Promise<unknown>) {
  busy.value = true
  busyText.value = text
  try {
    await task()
    await load()
  } catch (error) {
    if (error !== 'cancel' && error !== 'close') ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
function installRuntime() {
  return run('正在下载便携 Python（25–45MB），只放在应用数据目录…', async () => {
    await inkConfirm(
      '从 python-build-standalone 下载固定版本的 Python 3.12（约 25–45MB），校验后放在应用数据目录，不改动系统。继续吗？',
      '下载便携 Python',
      {
        confirmButtonText: '下载',
        cancelButtonText: '取消'
      }
    )
    await agentCommand('skill_runtime_install')
    ElMessage.success('便携 Python 已就绪')
  })
}
function installDeps() {
  return run('正在建立独立环境并安装依赖…', async () => {
    await agentCommand('skill_deps_install', { id: props.skill.id })
    ElMessage.success('依赖已安装')
  })
}
async function openReview(mode: 'view' | 'approve') {
  reviewMode.value = mode
  try {
    preview.value = await agentCommand<AgentSkillPreview>('skill_inspect', { id: props.skill.id })
    reviewVisible.value = true
  } catch (error) {
    ElMessage.error(String(error))
  }
}
async function openFolder() {
  try {
    await agentCommand('skill_open_folder', { id: props.skill.id })
  } catch (cause) {
    ElMessage.error(String(cause))
  }
}
async function copyPath() {
  try {
    await navigator.clipboard.writeText(status.value?.directory || props.skill.filePath || '')
    ElMessage.success('路径已复制')
  } catch {
    ElMessage.error('复制失败，请从上方选择路径复制')
  }
}
async function refresh() {
  try {
    await store.reload()
    await load()
  } catch (cause) {
    error.value = String(cause)
  }
}
function approve() {
  const hash = preview.value?.scriptHash
  if (!hash) return
  return run('正在记录确认…', async () => {
    await agentCommand('skill_approve', { id: props.skill.id, scriptHash: hash })
    reviewVisible.value = false
    await store.reload()
    ElMessage.success('已允许运行这一版脚本')
  })
}
watch(
  () => props.skill.id,
  () => void load(),
  { immediate: true }
)
</script>
