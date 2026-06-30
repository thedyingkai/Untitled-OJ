<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

const props = defineProps<{
  permission: string
}>()

const className = computed(() => {
  if (props.permission.startsWith('system.')) return 'permission-system'
  if (props.permission.startsWith('problem.')) return 'permission-problem'
  if (props.permission.startsWith('judge.')) return 'permission-judge'
  return 'permission-default'
})
</script>

<template>
  <NTag size="small" :class="['ojos-tag', className]">
    {{ permission }}
  </NTag>
</template>
