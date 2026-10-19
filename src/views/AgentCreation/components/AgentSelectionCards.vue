<template>
  <section class="ac-selection" aria-label="选题卡">
    <p class="ac-muted">
      “已核对”的内容由系统直接从本地榜单快照读出，不是模型的转述；“判断”是模型的归纳，请自行斟酌；
      “待核实”表示引用的快照或书名在本地找不到。
    </p>
    <article v-for="(card, index) in cards" :key="index" class="ac-selection-card">
      <header>
        <h3>{{ card.genre }}</h3>
        <span v-if="unverifiedCount(card)" class="ac-selection-flag">待核实 {{ unverifiedCount(card) }}</span>
      </header>
      <dl>
        <dt>卖点</dt>
        <dd>{{ card.sellingPoint }}</dd>
        <dt>主角与目标</dt>
        <dd>{{ card.protagonist }}</dd>
        <dt>开局</dt>
        <dd>{{ card.opening }}</dd>
        <template v-if="card.benchmarks.length">
          <dt>对标书</dt>
          <dd>{{ card.benchmarks.join('、') }}</dd>
        </template>
        <template v-if="card.risks.length">
          <dt>风险</dt>
          <dd>{{ card.risks.join('、') }}</dd>
        </template>
      </dl>
      <h4>证据</h4>
      <ul class="ac-selection-evidence">
        <li v-for="(item, itemIndex) in card.evidence" :key="itemIndex" :class="statusOf(item)">
          <span class="ac-selection-mark">{{ markLabel(statusOf(item)) }}</span>
          <div v-if="item.kind === 'data'">
            <p>{{ item.check?.text || `榜单 ${item.sourceId}（${item.snapshotDate} 快照）` }}</p>
            <small v-if="statusOf(item) === 'unverified'" class="ac-selection-issue">
              {{ item.check?.issue || '尚未核对' }}
            </small>
          </div>
          <div v-else>
            <p>{{ item.fact }}</p>
            <small>{{ item.source }}</small>
          </div>
        </li>
      </ul>
      <button class="ac-button ink-btn ink-btn-outline" type="button" :disabled="disabled" @click="$emit('use', index)">
        按这个方向开书
      </button>
    </article>
  </section>
</template>

<script setup lang="ts">
import type { AgentEvidenceCheck, AgentSelectionCard } from '@/types/agent-creation'

type EvidenceItem = AgentSelectionCard['evidence'][number]
defineProps<{ cards: AgentSelectionCard[]; disabled?: boolean }>()
defineEmits<{ use: [index: number] }>()
// 数据证据只有宿主核对通过才算已核对；旧成果或未核对的一律按待核实处理
const statusOf = (item: EvidenceItem): AgentEvidenceCheck['status'] =>
  item.kind === 'judgment' ? 'judgment' : item.check?.status === 'verified' ? 'verified' : 'unverified'
const markLabel = (status: AgentEvidenceCheck['status']) =>
  ({ verified: '已核对', unverified: '待核实', judgment: '判断' })[status]
const unverifiedCount = (card: AgentSelectionCard) =>
  card.evidence.filter(item => statusOf(item) === 'unverified').length
</script>
