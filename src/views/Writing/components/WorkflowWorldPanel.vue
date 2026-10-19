<template>
  <section class="workflow-asset-panel" aria-label="世界观管理">
    <div class="asset-toolbar">
      <el-input
        v-model="searchText"
        class="ink-select"
        clearable
        :disabled="loading"
        placeholder="搜索世界观设定"
      >
        <template #prefix>
          <i class="fa-solid fa-magnifying-glass"></i>
        </template>
      </el-input>
      <button
        type="button"
        class="ink-btn ink-btn-accent ink-btn-sm asset-create-button"
        :disabled="interactionDisabled"
        @click="beginCreate"
      >
        <i class="fa-solid fa-plus"></i>
        新增设定
      </button>
    </div>

    <div class="asset-list-shell">
      <template v-if="loading">
        <el-skeleton :rows="3" animated />
      </template>
      <div v-else-if="loadError" class="asset-state is-error">
        <i class="fa-solid fa-circle-exclamation"></i>
        <span>{{ loadError }}</span>
        <button type="button" class="ink-btn ink-btn-ghost ink-btn-sm" @click="loadWorldSettings()">
          重新加载
        </button>
      </div>
      <div v-else-if="!filteredSettings.length" class="asset-state">
        <i class="fa-solid fa-globe"></i>
        <span>{{ settings.length ? '没有匹配的世界观设定' : '还没有世界观设定，新增后将从下一章参与生成' }}</span>
      </div>
      <div v-else class="asset-list custom-scroll">
        <div
          v-for="setting in filteredSettings"
          :key="setting.id"
          class="asset-row"
          :class="{ active: !creating && selectedSettingId === setting.id }"
        >
          <button
            class="asset-row-main"
            type="button"
            :disabled="interactionDisabled"
            @click="selectSetting(setting)"
          >
            <span class="asset-icon">
              <i :class="typeIcon(setting.type)"></i>
            </span>
            <span class="asset-row-copy">
              <strong>{{ setting.name }}</strong>
              <small>{{ groupLabel(setting.groupId) }}</small>
            </span>
            <span class="asset-kind">{{ typeLabel(setting.type) }}</span>
          </button>
          <button
            type="button"
            class="asset-delete-button"
            :disabled="interactionDisabled && deletingSettingId !== setting.id"
            :aria-label="`删除设定${setting.name}`"
            @click.stop="deleteSetting(setting)"
          >
            <i :class="deletingSettingId === setting.id ? 'fa-solid fa-spinner fa-spin' : 'fa-regular fa-trash-can'"></i>
          </button>
        </div>
      </div>
    </div>

    <div class="asset-divider"></div>

    <div v-if="formVisible" class="asset-detail">
      <div class="asset-detail-head">
        <div>
          <strong>{{ creating ? '新增世界观设定' : '设定信息' }}</strong>
          <span>保存后从下一章起参与生成</span>
        </div>
        <span v-if="dirty" class="asset-dirty">待保存</span>
      </div>

      <div class="asset-form custom-scroll">
        <label class="form-field">
          <span>设定名称</span>
          <el-input
            v-model="form.name"
            class="ink-select"
            maxlength="100"
            :disabled="formDisabled"
            placeholder="输入设定名称"
          />
        </label>

        <div class="form-grid">
          <label class="form-field">
            <span>设定类型</span>
            <el-select
              v-model="form.type"
              class="ink-select workflow-select"
              popper-class="ink-select-popper"
              :disabled="formDisabled"
              fit-input-width
            >
              <el-option v-for="option in typeOptions" :key="option.value" :label="option.label" :value="option.value" />
            </el-select>
          </label>
          <label class="form-field">
            <span>所属分组</span>
            <el-select
              v-model="form.groupId"
              class="ink-select workflow-select"
              popper-class="ink-select-popper"
              :disabled="formDisabled"
              fit-input-width
            >
              <el-option label="未分组" value="" />
              <el-option v-for="group in groupOptions" :key="group.value" :label="group.label" :value="group.value" />
            </el-select>
          </label>
        </div>

        <label class="form-field form-field-detail">
          <span>设定详情</span>
          <div class="asset-textarea-wrap">
            <textarea
              v-model="form.detail"
              class="ink-input asset-textarea"
              rows="12"
              maxlength="5000"
              :disabled="formDisabled"
              placeholder="记录规则、限制、势力关系和会影响正文的关键信息"
            ></textarea>
            <span>{{ form.detail.length }} / 5000</span>
          </div>
        </label>
      </div>

      <div class="asset-actions">
        <button
          type="button"
          class="ink-btn ink-btn-outline"
          :disabled="formDisabled || !dirty"
          @click="discardDraft"
        >
          取消
        </button>
        <button
          type="button"
          class="ink-btn ink-btn-primary"
          :disabled="!canSave"
          @click="saveSetting"
        >
          <i v-if="saving" class="fa-solid fa-spinner fa-spin"></i>
          {{ saving ? '正在保存' : '保存设定' }}
        </button>
      </div>
    </div>

    <div v-else class="asset-state asset-detail-empty">
      <i class="fa-regular fa-hand-pointer"></i>
      <span>选择一项世界观设定查看和修改</span>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
// 开源版：世界观设定走本地参考库，函数形状对齐原服务端接口，调用点不动
import {
  addLocalWorldSetting as addWorldSettingApi,
  deleteLocalWorldSettings as deleteWorldSettingApi,
  getLocalWorldSettingTree as getWorldSettingTreeApi,
  updateLocalWorldSetting as updateWorldSettingApi,
} from '@/storage/local-reference'
import { showApiError } from '@/utils/api-error'
import type { WorldSetting, WorldSettingTreeFolder } from '@/types'
import { inkConfirm } from '@/utils/ink-confirm'

interface WorldSettingForm {
  name: string
  type: number
  groupId: string
  detail: string
}

interface WorldSettingGroupOption {
  label: string
  value: string
}

const props = defineProps<{
  bookId?: string | number
}>()

const emit = defineEmits<{
  (event: 'lore-updated'): void
}>()

const typeOptions = [
  { label: '地理位置', value: 0, icon: 'fa-solid fa-location-dot' },
  { label: '势力宗门', value: 1, icon: 'fa-solid fa-people-group' },
  { label: '功法技能', value: 2, icon: 'fa-solid fa-wand-magic-sparkles' },
  { label: '物品道具', value: 3, icon: 'fa-solid fa-gem' },
  { label: '等级体系', value: 4, icon: 'fa-solid fa-layer-group' },
  { label: '其他', value: 5, icon: 'fa-solid fa-bookmark' },
]

const createEmptyForm = (): WorldSettingForm => ({
  name: '',
  type: 5,
  groupId: '',
  detail: '',
})

const toSettingForm = (setting: WorldSetting): WorldSettingForm => ({
  name: setting.name,
  type: setting.type,
  groupId: setting.groupId === null || setting.groupId === undefined ? '' : String(setting.groupId),
  detail: setting.detail ?? '',
})

// 服务端世界观树是已保存基准，本地表单只承载当前尚未提交的编辑草稿。
const settingTree = ref<WorldSettingTreeFolder[]>([])
const selectedSettingId = ref<number | null>(null)
const previousSettingId = ref<number | null>(null)
const searchText = ref('')
const loading = ref(false)
const saving = ref(false)
const deletingSettingId = ref<number | null>(null)
const loadError = ref('')
const creating = ref(false)
const form = reactive<WorldSettingForm>(createEmptyForm())
let loadRevision = 0

const settings = computed(() => settingTree.value.flatMap(group => group.children))
const groupOptions = computed<WorldSettingGroupOption[]>(() =>
  settingTree.value
    .filter(group => !group.isVirtual)
    .map(group => ({ label: group.title, value: String(group.id) }))
)
const selectedSetting = computed(() =>
  settings.value.find(setting => setting.id === selectedSettingId.value) ?? null
)
const formVisible = computed(() => creating.value || selectedSetting.value !== null)
const interactionDisabled = computed(() => loading.value || saving.value || deletingSettingId.value !== null)
const formDisabled = computed(() => interactionDisabled.value || !formVisible.value)
const filteredSettings = computed(() => {
  const keyword = searchText.value.trim().toLocaleLowerCase()
  if (!keyword) return settings.value
  return settings.value.filter(setting =>
    [setting.name, setting.detail, typeLabel(setting.type), groupLabel(setting.groupId)]
      .filter(Boolean)
      .some(value => String(value).toLocaleLowerCase().includes(keyword))
  )
})
const dirty = computed(() => {
  if (creating.value) return true
  if (!selectedSetting.value) return false
  // 表单快照只比较可编辑字段，分组 ID 统一为字符串以匹配选择器值域。
  return JSON.stringify(form) !== JSON.stringify(toSettingForm(selectedSetting.value))
})
const canSave = computed(() =>
  Boolean(props.bookId && form.name.trim() && dirty.value && !formDisabled.value)
)

const typeLabel = (type: number) => typeOptions.find(option => option.value === type)?.label ?? '其他'
const typeIcon = (type: number) => typeOptions.find(option => option.value === type)?.icon ?? 'fa-solid fa-bookmark'
const groupLabel = (groupId?: string | number | null) => {
  if (groupId === null || groupId === undefined) return '未分组'
  return groupOptions.value.find(group => group.value === String(groupId))?.label ?? '未分组'
}

const assignForm = (nextForm: WorldSettingForm) => {
  Object.assign(form, nextForm)
}

const loadWorldSettings = async (preferredSettingId: number | null = selectedSettingId.value) => {
  if (!props.bookId) {
    settingTree.value = []
    selectedSettingId.value = null
    loadError.value = ''
    return
  }
  const revision = ++loadRevision
  loading.value = true
  loadError.value = ''
  try {
    const { data } = await getWorldSettingTreeApi(
      { bookId: props.bookId },
      { loading: false, silentError: true }
    )
    // 切换作品后丢弃旧请求结果，避免上一部作品的世界观写入当前面板。
    if (revision !== loadRevision) return
    settingTree.value = data ?? []
    const nextSetting =
      settings.value.find(setting => setting.id === preferredSettingId) ??
      settings.value[0] ??
      null
    selectedSettingId.value = nextSetting?.id ?? null
    creating.value = false
    assignForm(nextSetting ? toSettingForm(nextSetting) : createEmptyForm())
  } catch (error) {
    if (revision !== loadRevision) return
    loadError.value = '世界观加载失败'
    showApiError(error, loadError.value)
  } finally {
    if (revision === loadRevision) loading.value = false
  }
}

const confirmDiscard = async () => {
  if (!dirty.value) return true
  try {
    await inkConfirm('当前设定有未保存的修改，确定放弃吗？', '放弃修改', {
      confirmButtonText: '放弃',
      cancelButtonText: '继续编辑',
      type: 'warning',
    })
    return true
  } catch {
    return false
  }
}

const selectSetting = async (setting: WorldSetting) => {
  if (interactionDisabled.value || (!creating.value && selectedSettingId.value === setting.id)) return
  if (!(await confirmDiscard())) return
  creating.value = false
  selectedSettingId.value = setting.id
  previousSettingId.value = null
  assignForm(toSettingForm(setting))
}

const beginCreate = async () => {
  if (!props.bookId) {
    ElMessage.warning('请先选择要编辑的作品')
    return
  }
  if (interactionDisabled.value || !(await confirmDiscard())) return
  previousSettingId.value = selectedSettingId.value
  selectedSettingId.value = null
  creating.value = true
  assignForm(createEmptyForm())
}

const discardDraft = () => {
  if (creating.value) {
    const previousSetting =
      settings.value.find(setting => setting.id === previousSettingId.value) ??
      settings.value[0] ??
      null
    selectedSettingId.value = previousSetting?.id ?? null
    previousSettingId.value = null
    creating.value = false
    assignForm(previousSetting ? toSettingForm(previousSetting) : createEmptyForm())
    return
  }
  if (selectedSetting.value) assignForm(toSettingForm(selectedSetting.value))
}

const saveSetting = async () => {
  if (!canSave.value || !props.bookId) return
  const name = form.name.trim()
  if (!name) {
    ElMessage.warning('请输入设定名称')
    return
  }
  const wasCreating = creating.value
  saving.value = true
  try {
    const payload = {
      name,
      type: form.type,
      groupId: form.groupId || null,
      detail: form.detail.trim(),
    }
    const { data } = wasCreating
      ? await addWorldSettingApi({ bookId: String(props.bookId), ...payload })
      : await updateWorldSettingApi({ id: selectedSettingId.value!, ...payload }, selectedSetting.value || undefined)
    await loadWorldSettings(data.id)
    emit('lore-updated')
    ElMessage.success(wasCreating ? '设定已新增' : '设定已保存')
  } catch (error) {
    showApiError(error, wasCreating ? '新增设定失败' : '保存设定失败')
  } finally {
    saving.value = false
  }
}

const deleteSetting = async (setting: WorldSetting) => {
  if (interactionDisabled.value) return
  try {
    await inkConfirm(`确定删除设定「${setting.name}」吗？`, '删除设定', {
      confirmButtonText: '删除',
      cancelButtonText: '取消',
      type: 'warning',
    })
  } catch {
    return
  }
  deletingSettingId.value = setting.id
  try {
    await deleteWorldSettingApi({ ids: [setting.id] })
    settingTree.value = settingTree.value.map(group => ({
      ...group,
      children: group.children.filter(candidate => candidate.id !== setting.id),
    }))
    // 删除当前设定时选择下一项；删除其他设定不打断正在填写的表单草稿。
    if (!creating.value && selectedSettingId.value === setting.id) {
      const nextSetting = settings.value[0] ?? null
      selectedSettingId.value = nextSetting?.id ?? null
      assignForm(nextSetting ? toSettingForm(nextSetting) : createEmptyForm())
    }
    emit('lore-updated')
    ElMessage.success('设定已删除')
  } catch (error) {
    showApiError(error, '删除设定失败')
  } finally {
    deletingSettingId.value = null
  }
}

watch(
  () => props.bookId,
  () => {
    loadRevision += 1
    searchText.value = ''
    settingTree.value = []
    selectedSettingId.value = null
    previousSettingId.value = null
    creating.value = false
    assignForm(createEmptyForm())
    void loadWorldSettings()
  },
  { immediate: true }
)
</script>

<style scoped lang="scss">
@use './workflow-asset-panel';

.form-field-detail {
  min-height: 0;
}

.asset-textarea-wrap {
  height: 100%;
}

.asset-textarea {
  min-height: 220px;
}

@media (max-height: 720px) {
  .asset-textarea {
    min-height: 160px;
  }
}
</style>
