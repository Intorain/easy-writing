<template>
          <section class="settings-pane">
            <div class="overview-status-grid">
              <article class="overview-status-card">
                <span class="status-icon">
                  <i :class="themeStore.themeConfig.icon"></i>
                </span>
                <div>
                  <span>当前主题</span>
                  <strong>{{ themeStore.themeConfig.label }}</strong>
                  <small>{{ currentSkinLabel }}</small>
                </div>
                <button type="button" class="mini-outline-btn" @click="ctx.navigate('appearance')">更换主题</button>
              </article>
              <article class="overview-status-card">
                <span class="status-icon sync">
                  <i class="fa-solid fa-cloud-arrow-up"></i>
                </span>
                <div>
                  <span>保存状态</span>
                  <strong>本地自动保存</strong>
                  <small>上次备份：{{ formatLocaleDateTime(settingsDraft.lastBackupAt, '暂无') }}</small>
                </div>
                <button type="button" class="mini-outline-btn" @click="ctx.navigate('sync')">管理保存</button>
              </article>
              <article class="overview-status-card">
                <span class="status-icon model model-status-icon">
                  <img v-if="defaultModelIcon" :src="defaultModelIcon" alt="" />
                  <i v-else class="fa-solid fa-wand-magic-sparkles"></i>
                </span>
                <div>
                  <span>默认模型</span>
                  <strong>写作 · {{ defaultTextModelLabel }}</strong>
                  <small>建书 {{ defaultWorkflowModelLabel }} · 生图 {{ defaultImageModelLabel }}</small>
                </div>
                <button type="button" class="mini-outline-btn" @click="goAiModels">更换模型</button>
              </article>
            </div>

            <div class="shortcut-grid">
              <button
                v-for="item in shortcuts"
                :key="item.id"
                type="button"
                class="shortcut-card"
                @click="ctx.navigate(item.id)"
              >
                <i :class="item.icon"></i>
                <strong>{{ item.title }}</strong>
                <span>{{ item.description }}</span>
              </button>
            </div>

            <section class="settings-card">
              <div class="settings-card-title">
                <i class="fa-solid fa-sliders"></i>
                <strong>推荐调整</strong>
              </div>
              <div class="recommend-list">
                <label class="recommend-row">
                  <span><i class="fa-solid fa-keyboard"></i>打字音效</span>
                  <el-switch v-model="typingSoundEnabled" />
                </label>
                <label class="recommend-row">
                  <span><i class="fa-solid fa-cloud-arrow-up"></i>自动备份</span>
                  <el-switch v-model="settingsDraft.backupEnabled" :disabled="!desktopSupported" />
                </label>
              </div>
            </section>
          </section>
</template>

<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useAiModelStore } from '@/stores/ai-model'
import { useThemeStore } from '@/stores/theme'
import { formatLocaleDateTime } from '@/utils/format'
import type { SettingsSectionId } from '@/types/settings-center'
import { useSettingsCenterCtx } from '../settings-context'

const ctx = useSettingsCenterCtx()
const { settingsDraft } = ctx
const desktopSupported = ctx.desktopSupported
const router = useRouter()
const themeStore = useThemeStore()
const aiModelStore = useAiModelStore()

const currentSkinLabel = computed(() => themeStore.currentSkinObj?.name || '默认背景')

const modelProviderIcons: Record<string, string> = {
  openai: '/provider-icons/openai.svg',
  deepseek: '/provider-icons/deepseek.ico',
  aliyun: '/provider-icons/tongyi.svg',
  volcengine: '/provider-icons/volcengine.png',
  bigmodel: '/provider-icons/bigmodel.png',
  siliconflow: '/provider-icons/siliconflow.ico',
  openrouter: '/provider-icons/openrouter.ico',
  gemini_openai: '/provider-icons/gemini.svg',
  claude: '/provider-icons/claude.svg',
  xai: '/provider-icons/xai.svg',
  local: '/provider-icons/local.svg',
}

const normalizeModelProvider = (provider?: string, code?: string) => {
  const text = `${provider || ''} ${code || ''}`.toLowerCase()
  // claude/gemini 必须先于 openai 判断：gemini_openai、openai-gpt-* 都含 "openai"
  if (text.includes('claude') || text.includes('anthropic')) return 'claude'
  if (text.includes('gemini')) return 'gemini_openai'
  if (text.includes('grok') || text.includes('xai')) return 'xai'
  if (text.includes('deepseek')) return 'deepseek'
  if (text.includes('openai') || text.includes('gpt')) return 'openai'
  if (text.includes('qwen') || text.includes('tongyi') || text.includes('aliyun')) return 'aliyun'
  if (text.includes('doubao') || text.includes('volc') || text.includes('ark')) return 'volcengine'
  if (text.includes('glm') || text.includes('zhipu') || text.includes('bigmodel')) return 'bigmodel'
  if (text.includes('silicon')) return 'siliconflow'
  if (text.includes('openrouter')) return 'openrouter'
  return provider || ''
}

// 三组默认模型：用户组偏好 → 记录的选中值 → 平台策略默认
const getGroupDefaultModel = (groupCode: 'text_assist' | 'workflow_book' | 'image_generation') => {
  // 实际生效的模型：没选时写作助手/生图自动用第一个启用的，工作流没选就是空
  const selectedCode = aiModelStore.effectiveModelCode(groupCode)
  const group = aiModelStore.groups[groupCode]
  return (group?.models || []).find(item => item.code === selectedCode)
    || group?.selectedModel
    || group?.defaultModel
    || null
}
const defaultTextModelLabel = computed(() => getGroupDefaultModel('text_assist')?.name || '未设置')
const defaultWorkflowModelLabel = computed(() => getGroupDefaultModel('workflow_book')?.name || '未设置')
const defaultImageModelLabel = computed(() => getGroupDefaultModel('image_generation')?.name || '未设置')
const defaultModelIcon = computed(() => {
  const model = getGroupDefaultModel('text_assist')
  if (!model) return ''
  const provider = normalizeModelProvider(model.provider, `${model.name || ''} ${model.modelCode || model.code || ''}`)
  return modelProviderIcons[provider] || ''
})

const typingSoundEnabled = computed({
  get: () => ctx.editorDraft.typingSound !== 'none',
  set: value => {
    ctx.editorDraft.typingSound = value ? 'typewriter' : 'none'
  },
})

const shortcuts: Array<{ id: SettingsSectionId; icon: string; title: string; description: string }> = [
  { id: 'writing', icon: 'fa-regular fa-pen-to-square', title: '写作体验', description: '配置打字反馈、自爆挑战与写作辅助' },
  { id: 'appearance', icon: 'fa-solid fa-palette', title: '外观主题', description: '切换主题风格、字体与排版效果' },
  { id: 'record', icon: 'fa-regular fa-clipboard', title: 'AI 调用记录', description: '查看模型调用、工作流生成与会话记录' },
  { id: 'weixin', icon: 'fa-brands fa-weixin', title: '微信助手', description: '绑定微信 ClawBot，在微信里查榜单与灵感' },
  { id: 'cache', icon: 'fa-solid fa-database', title: '存储空间', description: '查看本地缓存占用并清理临时数据' },
]

const goAiModels = async () => {
  ctx.close()
  await router.push('/aiModels')
}

onMounted(() => {
  void aiModelStore.loadAll()
})
</script>
