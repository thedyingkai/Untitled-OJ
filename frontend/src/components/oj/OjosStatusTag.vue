<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

import {
  getHealthStatusMeta,
  getJudgeStatusMeta,
  getModuleStatusMeta,
  getProblemStatusMeta,
  getTaskStatusMeta,
  getWorkerStatusMeta,
  type StatusMeta,
} from '../../utils/status'

const props = withDefaults(
  defineProps<{
    status: string
    domain?: 'judge' | 'health' | 'worker' | 'module' | 'task' | 'problem'
    size?: 'small' | 'medium' | 'large'
  }>(),
  {
    domain: 'judge',
    size: 'small',
  },
)

const meta = computed<StatusMeta>(() => {
  if (props.domain === 'health') return getHealthStatusMeta(props.status)
  if (props.domain === 'worker') return getWorkerStatusMeta(props.status)
  if (props.domain === 'module') return getModuleStatusMeta(props.status)
  if (props.domain === 'task') return getTaskStatusMeta(props.status)
  if (props.domain === 'problem') return getProblemStatusMeta(props.status)
  return getJudgeStatusMeta(props.status)
})
</script>

<template>
  <NTag
    :type="meta.type"
    :size="size"
    :class="['ojos-tag', meta.className]"
    :title="meta.description"
  >
    {{ meta.label }}
  </NTag>
</template>
