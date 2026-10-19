<!-- components/SmartPopover.vue - 智能悬浮面板组件 -->
<template>
  <div class="smart-popover-wrapper" ref="wrapperRef">
    <!-- 触发器 -->
    <div
      class="trigger-wrapper"
      ref="triggerRef"
      @click="handleClick"
      @mouseenter="handleMouseEnter"
      @mouseleave="handleMouseLeave"
    >
      <slot name="trigger"></slot>
    </div>

    <!-- 悬浮面板 -->
    <Teleport to="body">
      <Transition name="popover">
        <div
          v-if="visible"
          ref="popoverRef"
          class="smart-popover ink-popover-panel"
          :style="popoverStyle"
          @mouseenter="handleContentEnter"
          @mouseleave="handleContentLeave"
        >
          <slot></slot>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from 'vue'

type Placement = 'top' | 'top-start' | 'top-end' | 'bottom' | 'bottom-start' | 'bottom-end'
type Trigger = 'click' | 'hover'

const props = withDefaults(defineProps<{
  width?: number | string
  placement?: Placement
  trigger?: Trigger
  showAfter?: number
  hideAfter?: number
  offset?: number
}>(), {
  width: 320,
  placement: 'bottom',
  trigger: 'click',
  showAfter: 0,
  hideAfter: 200,
  offset: 8
})

const emit = defineEmits<{
  (e: 'update:visible', value: boolean): void
  (e: 'show'): void
  (e: 'hide'): void
}>()

const visible = ref(false)
// const wrapperRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
let resizeObserver: ResizeObserver | null = null

let hideTimer: ReturnType<typeof setTimeout> | null = null
let showTimer: ReturnType<typeof setTimeout> | null = null

// 计算弹出层位置
const popoverStyle = computed(() => {
  const width = typeof props.width === 'number' ? `${props.width}px` : props.width
  return { width }
})

// 更新弹出层位置
const updatePosition = () => {
  if (!triggerRef.value || !popoverRef.value) return

  const triggerRect = triggerRef.value.getBoundingClientRect()
  const popoverRect = popoverRef.value.getBoundingClientRect()
  const { placement, offset } = props

  let top = 0
  let left = 0

  // 计算垂直位置
  if (placement.startsWith('top')) {
    top = triggerRect.top - popoverRect.height - offset
  } else {
    top = triggerRect.bottom + offset
  }

  // 计算水平位置
  if (placement.endsWith('-start')) {
    left = triggerRect.left
  } else if (placement.endsWith('-end')) {
    left = triggerRect.right - popoverRect.width
  } else {
    left = triggerRect.left + (triggerRect.width - popoverRect.width) / 2
  }

  // 边界检测
  const viewportWidth = window.innerWidth
  const viewportHeight = window.innerHeight

  // 水平边界
  if (left < 10) left = 10
  if (left + popoverRect.width > viewportWidth - 10) {
    left = viewportWidth - popoverRect.width - 10
  }

  // 垂直边界 - 如果下方空间不足，尝试放到上方
  if (placement.startsWith('bottom') && top + popoverRect.height > viewportHeight - 10) {
    const topSpace = triggerRect.top - offset
    const bottomSpace = viewportHeight - (triggerRect.bottom + offset)

    // 如果上方空间比下方大，且下方放不下，则放到上方
    if (topSpace > bottomSpace) {
      top = triggerRect.top - popoverRect.height - offset
    }
  }
  // 如果上方空间不足，放到下方
  else if (placement.startsWith('top') && top < 10) {
    top = triggerRect.bottom + offset
  }

  // 两侧空间都不足时仍留在窗口内；可滚动的偏好面板不能被翻到屏幕上方。
  top = Math.max(10, Math.min(top, viewportHeight - popoverRect.height - 10))

  popoverRef.value.style.top = `${top}px`
  popoverRef.value.style.left = `${left}px`
}

// 显示面板
const show = () => {
  if (hideTimer) {
    clearTimeout(hideTimer)
    hideTimer = null
  }

  if (props.showAfter > 0) {
    showTimer = setTimeout(() => {
      visible.value = true
    }, props.showAfter)
  } else {
    visible.value = true
  }
}

// 隐藏面板
const hide = () => {
  if (showTimer) {
    clearTimeout(showTimer)
    showTimer = null
  }

  if (props.hideAfter > 0) {
    hideTimer = setTimeout(() => {
      visible.value = false
    }, props.hideAfter)
  } else {
    visible.value = false
  }
}

// 切换显示/隐藏
const toggle = () => {
  if (visible.value) {
    visible.value = false
  } else {
    show()
  }
}

// 点击触发器
const handleClick = () => {
  if (props.trigger === 'click') {
    toggle()
  }
}

// 鼠标进入触发器
const handleMouseEnter = () => {
  if (props.trigger === 'hover') {
    show()
  }
}

// 鼠标离开触发器
const handleMouseLeave = () => {
  if (props.trigger === 'hover') {
    hide()
  }
}

// 鼠标进入内容区
const handleContentEnter = () => {
  if (props.trigger === 'hover') {
    if (hideTimer) {
      clearTimeout(hideTimer)
      hideTimer = null
    }
  }
}

// 鼠标离开内容区
const handleContentLeave = () => {
  if (props.trigger === 'hover') {
    hide()
  }
}

// 点击外部关闭
const handleClickOutside = (event: MouseEvent) => {
  if (!visible.value) return

  const target = event.target as HTMLElement

  // 如果点击的是触发器内部，不处理
  if (triggerRef.value?.contains(target)) return

  // 如果点击的是弹出层内部，不关闭
  if (popoverRef.value?.contains(target)) return

  // 如果点击的是 Element Plus 的弹出层，不关闭
  if (target.closest('.el-popper') ||
      target.closest('.el-select__popper') ||
      target.closest('.el-color-picker__panel') ||
      target.closest('.el-slider__button-wrapper')) {
    return
  }

  visible.value = false
}

// 窗口resize时更新位置
const handleResize = () => {
  if (visible.value) {
    updatePosition()
  }
}

// 滚动时更新位置
const handleScroll = () => {
  if (visible.value) {
    updatePosition()
  }
}

// 监听visible变化，更新位置
watch(visible, (val) => {
  emit('update:visible', val)
  if (val) {
    emit('show')
    nextTick(() => {
      updatePosition()
      // 设置 ResizeObserver
      if (popoverRef.value && !resizeObserver) {
        resizeObserver = new ResizeObserver(() => {
          updatePosition()
        })
        resizeObserver.observe(popoverRef.value)
      }
    })
  } else {
    emit('hide')
    // 清理 ResizeObserver
    if (resizeObserver) {
      resizeObserver.disconnect()
      resizeObserver = null
    }
  }
})

onMounted(() => {
  window.addEventListener('click', handleClickOutside, true)
  window.addEventListener('resize', handleResize)
  window.addEventListener('scroll', handleScroll, true)
})

onBeforeUnmount(() => {
  window.removeEventListener('click', handleClickOutside, true)
  window.removeEventListener('resize', handleResize)
  window.removeEventListener('scroll', handleScroll, true)

  if (hideTimer) clearTimeout(hideTimer)
  if (showTimer) clearTimeout(showTimer)

  if (resizeObserver) {
    resizeObserver.disconnect()
    resizeObserver = null
  }
})

// 暴露方法
defineExpose({
  show,
  hide,
  toggle
})
</script>

<style lang="scss">
.smart-popover-wrapper {
  display: inline-block;
}

.trigger-wrapper {
  display: inline-block;
  cursor: pointer;
}

.smart-popover.ink-popover-panel {
  position: fixed;
  /* 视觉样式由 .ink-popover-panel 统一管理 (背景、边框、阴影、圆角、模糊等) */
  /* background, border, shadow, backdrop-filter, border-radius provided by ink-popover-panel */
}

/* 过渡动画 */
.popover-enter-active {
  transition: opacity 0.2s ease, transform 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.popover-leave-active {
  transition: opacity 0.15s ease, transform 0.15s ease;
}

.popover-enter-from {
  opacity: 0;
  transform: translateY(-8px);
}

.popover-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}
</style>
