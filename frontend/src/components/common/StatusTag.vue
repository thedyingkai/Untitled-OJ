<script setup lang="ts">
import OjosStatusTag from '../oj/OjosStatusTag.vue'

withDefaults(
  defineProps<{
    status: string
    domain?: 'judge' | 'health' | 'worker' | 'module' | 'task' | 'problem'
  }>(),
  {
    domain: 'judge',
  },
)
</script>

<template>
  <OjosStatusTag :status="status" :domain="domain" />
</template>
