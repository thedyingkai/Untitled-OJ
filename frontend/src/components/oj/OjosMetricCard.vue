<script setup lang="ts">
import OjosStatCard from './OjosStatCard.vue'

withDefaults(
  defineProps<{
    label: string
    value: string | number
    note?: string
    tone?: 'default' | 'primary' | 'success' | 'warning' | 'danger'
  }>(),
  {
    note: '',
    tone: 'default',
  },
)
</script>

<template>
  <OjosStatCard :label="label" :value="value" :note="note" :tone="tone" />
</template>
