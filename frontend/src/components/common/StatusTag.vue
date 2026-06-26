<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

import type { JudgeStatus } from '../../types/judge'

type TagType = 'default' | 'success' | 'warning' | 'error' | 'info'

const props = defineProps<{
  status: JudgeStatus | string
}>()

const statusMap: Record<string, { label: string; type: TagType }> = {
  PENDING: { label: 'Pending', type: 'default' },
  JUDGING: { label: 'Judging', type: 'info' },
  ACCEPTED: { label: 'Accepted', type: 'success' },
  WRONG_ANSWER: { label: 'Wrong Answer', type: 'error' },
  COMPILE_ERROR: { label: 'Compile Error', type: 'warning' },
  RUNTIME_ERROR: { label: 'Runtime Error', type: 'error' },
  TIME_LIMIT_EXCEEDED: { label: 'Time Limit', type: 'warning' },
  MEMORY_LIMIT_EXCEEDED: { label: 'Memory Limit', type: 'warning' },
  OUTPUT_LIMIT_EXCEEDED: { label: 'Output Limit', type: 'warning' },
  SYSTEM_ERROR: { label: 'System Error', type: 'error' },
  CANCELLED: { label: 'Cancelled', type: 'default' },
  UNSUPPORTED_LANGUAGE: { label: 'Unsupported', type: 'error' },
}

const meta = computed(() => statusMap[props.status] ?? { label: props.status, type: 'default' })
</script>

<template>
  <NTag :type="meta.type" size="small" round>
    {{ meta.label }}
  </NTag>
</template>
