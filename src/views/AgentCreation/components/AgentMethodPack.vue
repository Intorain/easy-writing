<template>
  <div class="ac-canon-section">
    <header class="ac-canon-section-head">
      <div>
        <h3>方法包</h3>
        <p>
          把全局写作规则、自建的平台画像、拆书归纳的写法配方和各写作方法的效果统计导出成一个文件，换电脑或分享时导入。
          不含任何正文、资料与记忆；导入只新增，重复的规则跳过，同编号的画像另存一份。
        </p>
      </div>
      <div class="ac-actions">
        <button class="ac-button ink-btn ink-btn-outline" type="button" :disabled="busy" @click="exportPack">导出</button
        ><button class="ac-button ink-btn ink-btn-outline" type="button" :disabled="busy" @click="importPack">导入</button>
      </div>
    </header>
    <p v-if="!templates.length" class="ac-small-empty">还没有导入的写法配方。</p>
    <template v-else>
      <p class="ac-muted">导入的写法配方（Agent 仿写时会参考）</p>
      <ul class="ac-recipe-list">
        <li v-for="recipe in templates" :key="recipe.id">
          <div>
            <strong>{{ recipe.title }}</strong>
            <small v-if="recipe.projectTitle" class="ac-muted">来自《{{ recipe.projectTitle }}》</small>
          </div>
          <ol>
            <li v-for="(step, index) in recipe.steps" :key="index">
              <span>{{ step.position }}</span>{{ step.tasks }}
            </li>
          </ol>
          <button class="ac-text-link" type="button" :disabled="busy" @click="remove(recipe)">删除</button>
        </li>
      </ul>
    </template>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { agentCommand } from '@/agent/client'
import { inkConfirm } from '@/utils/ink-confirm'

interface TemplateRecipe {
  id: string
  title: string
  projectTitle?: string
  steps: Array<{ position: string; tasks: string }>
}
interface ImportSummary {
  rules: { added: number; skipped: number }
  profiles: { added: number; skipped: number }
  recipes: { added: number; skipped: number }
}

const emit = defineEmits<{ imported: [] }>()
const busy = ref(false)
const templates = ref<TemplateRecipe[]>([])

async function load() {
  try {
    templates.value = await agentCommand<TemplateRecipe[]>('template_recipes')
  } catch {
    templates.value = []
  }
}
function stamp() {
  const now = new Date()
  return `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, '0')}${String(now.getDate()).padStart(2, '0')}`
}
async function exportPack() {
  const { save } = await import('@tauri-apps/plugin-dialog')
  const path = await save({ title: '导出方法包', defaultPath: `易创方法包-${stamp()}.json`, filters: [{ name: '方法包', extensions: ['json'] }] })
  if (!path) return
  busy.value = true
  try {
    const result = await agentCommand<{ rules: number; profiles: number; recipes: number }>('method_pack_export', { path })
    ElMessage.success(`已导出：规则 ${result.rules} 条、画像 ${result.profiles} 个、配方 ${result.recipes} 个`)
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
async function importPack() {
  const { open } = await import('@tauri-apps/plugin-dialog')
  const path = await open({ title: '导入方法包', multiple: false, directory: false, filters: [{ name: '方法包', extensions: ['json'] }] })
  if (!path || Array.isArray(path)) return
  busy.value = true
  try {
    const result = await agentCommand<ImportSummary>('method_pack_import', { path })
    ElMessage.success(
      `已导入：规则 ${result.rules.added} 条（跳过重复 ${result.rules.skipped}）、画像 ${result.profiles.added} 个、配方 ${result.recipes.added} 个`
    )
    await load()
    emit('imported')
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
async function remove(recipe: TemplateRecipe) {
  try {
    await inkConfirm(`删除配方「${recipe.title}」？`, '删除配方', { confirmButtonText: '删除', type: 'warning' })
  } catch {
    return
  }
  busy.value = true
  try {
    await agentCommand('delete_recipe', { id: recipe.id })
    await load()
  } catch (error) {
    ElMessage.error(String(error))
  } finally {
    busy.value = false
  }
}
onMounted(load)
</script>
