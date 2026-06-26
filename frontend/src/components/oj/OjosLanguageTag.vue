<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

const props = defineProps<{
  language: string
  enabled?: boolean
}>()

const label = computed(() => {
  const normalized = props.language.toLowerCase()
  if (normalized === 'cpp17') return 'C++17'
  if (normalized === 'cpp20') return 'C++20'
  if (normalized === 'c11') return 'C11'
  if (normalized === 'python3') return 'Python 3'
  if (normalized === 'java17') return 'Java 17'
  return props.language
})
</script>

<template>
  <NTag :type="enabled === false ? 'default' : 'info'" size="small" class="ojos-tag language-tag">
    {{ label }}
  </NTag>
</template>
