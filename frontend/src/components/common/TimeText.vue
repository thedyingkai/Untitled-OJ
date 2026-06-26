<script setup lang="ts">
import { computed } from 'vue'
import { NText } from 'naive-ui'

const props = defineProps<{
  value?: string | number | Date | null
}>()

const formatted = computed(() => {
  if (!props.value) {
    return '-'
  }
  const date = props.value instanceof Date ? props.value : new Date(props.value)
  if (Number.isNaN(date.getTime())) {
    return String(props.value)
  }
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(date)
})
</script>

<template>
  <NText :title="formatted">{{ formatted }}</NText>
</template>
