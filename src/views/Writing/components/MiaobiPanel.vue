<template>
  <div class="panel-content miaobi-panel" :class="{ 'is-expanded': isExpanded }">
    <div class="panel-header">
      <span class="panel-title text-ellipsis" :title="sessionTitle">{{ sessionTitle }}</span>
      <div class="panel-actions">
        <el-popover v-if="bookSessions.length > 1" placement="bottom-end" trigger="click" :width="260" popper-class="miaobi-sessions-popper">
          <template #reference>
            <i class="fa-solid fa-clock-rotate-left action-icon" title="这本书的对话"></i>
          </template>
          <ul class="miaobi-sessions">
            <li
              v-for="item in bookSessions"
              :key="item.id"
              :class="{ active: item.id === session?.id }"
              @click="select(item.id)"
            >
              <span class="text-ellipsis">{{ item.title }}</span>
              <small v-if="item.legacy">旧版</small>
            </li>
          </ul>
        </el-popover>
        <el-tooltip content="新对话" placement="bottom" :show-after="500">
          <i class="fa-solid fa-plus action-icon" @click="startNew"></i>
        </el-tooltip>
        <el-tooltip :content="isExpanded ? '收起' : '展开面板'" placement="bottom" :show-after="500">
          <i class="fa-solid fa-expand action-icon" :class="{ active: isExpanded }" @click="toggleExpand"></i>
        </el-tooltip>
        <i class="fa-solid fa-xmark action-icon" @click="$emit('close')"></i>
      </div>
    </div>
    <div v-if="!ready" class="miaobi-state" role="status">{{ error || '正在连接…' }}</div>
    <AgentDocument v-else-if="store.openedArtifact" :expanded="isExpanded" @expand="toggleExpand" />
    <section v-else class="ac-conversation-pane miaobi-pane" aria-label="妙笔对话">
      <div v-if="!hasMessages" class="miaobi-empty">
        <button v-for="item in quickPrompts" :key="item" type="button" class="miaobi-starter" @click="quickAsk(item)">
          {{ item }}
        </button>
      </div>
      <AgentConversation v-else @continue="focusComposer" />
      <div v-if="session?.legacy" class="ac-composer-dock ac-legacy-dock">
        <span>旧版妙笔的对话记录，只能查看。</span>
        <button type="button" class="ac-legacy-new" @click="startNew">开新对话</button>
      </div>
      <div v-else class="ac-composer-dock">
        <AgentComposer ref="composer" compact />
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, toRef } from 'vue'
import AgentConversation from '@/views/AgentCreation/components/AgentConversation.vue'
import AgentComposer from '@/views/AgentCreation/components/AgentComposer.vue'
import AgentDocument from '@/views/AgentCreation/components/AgentDocument.vue'
import '@/views/AgentCreation/style.scss'
import { useWritingEditorStore } from '@/stores/writing-editor'
import { useMiaobiSession } from '../composables/useMiaobiSession'

/**
 * 妙笔面板：Agent 板块那条作品会话在编辑器侧的视图，只做渲染与输入。
 * 对话、引导卡与确认卡、成果（采用与撤销）都是 Agent 板块的同一套组件；写操作先出确认卡。
 */

const props = defineProps<{ title: string; bookId?: string | number; bookTitle?: string }>()
defineEmits<{ (e: 'close'): void }>()

const writingStore = useWritingEditorStore()
const bookId = computed(() => (props.bookId == null || props.bookId === '' ? '' : String(props.bookId)))
const bookTitle = toRef(() => props.bookTitle || '')
const { store, ready, error, startNew, select, bookSessions, session, hasMessages } = useMiaobiSession(bookId, bookTitle)

const sessionTitle = computed(() => session.value?.title || (bookTitle.value ? `《${bookTitle.value}》` : props.title))
const composer = ref<InstanceType<typeof AgentComposer>>()
const focusComposer = () => nextTick(() => composer.value?.focus())

const quickPrompts = [
  '这一章的冲突够不够，下一拍该怎么接？',
  '主角现在的动机站得住吗？',
  '这段设定有没有穿帮或越界？',
  '这个开头的钩子够不够吸引人？',
]
const quickAsk = async (text: string) => {
  store.draft = text
  await store.send()
}

const isExpanded = ref(false)
const originalPanelWidth = ref<number | null>(null)
const toggleExpand = () => {
  if (isExpanded.value) {
    if (originalPanelWidth.value !== null) writingStore.setRightPanelWidth(originalPanelWidth.value)
    isExpanded.value = false
  } else {
    originalPanelWidth.value = writingStore.rightPanelWidth
    writingStore.setRightPanelWidth(Math.min(writingStore.rightPanelWidth + 160, 600))
    isExpanded.value = true
  }
}
</script>

<style scoped lang="scss">
.panel-content {
  flex: 1;
  display: flex;
  flex-direction: column;
  width: 100%;
  min-width: 0;
  height: 100%;
  background: transparent;
  .panel-header {
    height: 40px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0 12px;
    border-bottom: 1px solid var(--divider);
    background: var(--panel-header-bg);
    flex-shrink: 0;
    .panel-title {
      font-weight: bold;
      font-size: 14px;
      color: var(--ink-main);
      min-width: 0;
    }
    .panel-actions {
      display: flex;
      align-items: center;
      gap: 12px;
      flex-shrink: 0;
      .action-icon {
        font-size: 12px;
        color: var(--ink-sec);
        opacity: 0.6;
        cursor: pointer;
        transition: all 0.2s;
        &:hover,
        &.active {
          opacity: 1;
          color: var(--ink-main);
        }
      }
    }
  }
  .miaobi-state {
    padding: 24px 16px;
    color: var(--ink-sec);
    font-size: 13px;
  }
  .miaobi-empty {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 16px 12px;
    .miaobi-starter {
      text-align: left;
      padding: 10px 12px;
      border: 1px solid var(--divider);
      border-radius: 8px;
      background: transparent;
      color: var(--ink-main);
      font-size: 13px;
      line-height: 1.5;
      cursor: pointer;
      &:hover {
        background: var(--overlay-hover, rgba(36, 31, 27, 0.06));
      }
    }
  }
}
</style>

<style lang="scss">
/* 会话列表弹层渲染在组件之外，样式不能 scoped */
.miaobi-sessions-popper .miaobi-sessions {
  list-style: none;
  margin: 0;
  padding: 0;
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
    color: var(--ink-main);
    &:hover,
    &.active {
      background: var(--overlay-hover, rgba(36, 31, 27, 0.06));
    }
    small {
      color: var(--ink-sec);
      flex-shrink: 0;
    }
  }
}

/* 复用 Agent 板块的对话、输入框与成果面板：在窄侧栏里去掉内容最小宽度，成果面板占满面板 */
.miaobi-panel {
  .ac-conversation-pane,
  .ac-messages,
  .ac-composer-dock {
    --ac-content-min-width: 0;
    --ac-content-max-width: 100%;
    --ac-content-gutter: 12px;
  }
  .miaobi-pane {
    flex: 1;
    min-height: 0;
  }
  .ac-messages {
    padding-top: 12px;
  }
  /* 成果面板：正文编辑面占满剩余高度。Agent 板块的同名规则挂在 .ac-workspace 下，这里没有那一层，
     不补就落回全局输入框样式（带边框、可拖拽、只有两行高）；窄侧栏里边距也收紧。 */
  .ac-document {
    flex: 1;
    width: 100%;
    max-width: none;
    min-width: 0;
    min-height: 0;
    border-left: 0;
  }
  .ac-document > textarea.ac-document-editor {
    flex: 1;
    width: 100%;
    min-height: 0;
    padding: 16px 14px;
    border: 0;
    border-radius: 0;
    resize: none;
    background: transparent;
    box-shadow: none;
    font-size: 1rem;
    line-height: 1.9;
    outline: none;
    overflow-y: auto;
  }
  .ac-document > textarea.ac-document-editor:focus-visible {
    outline: 2px solid var(--ac-line);
    outline-offset: -5px;
  }
  .ac-document-header {
    padding: 16px 14px 10px;
  }
  .ac-document-header h2 {
    font-size: 1.125rem;
  }
  .ac-document-tabs,
  .ac-document-footer {
    margin: 0 14px;
  }
  .ac-document-preview,
  .ac-document-reading {
    padding: 16px 14px;
    font-size: 1rem;
    line-height: 1.9;
  }
  .ac-delivery-status {
    padding: 10px 14px;
  }
  .ac-quality {
    padding: 14px 14px 20px;
  }
}
</style>
