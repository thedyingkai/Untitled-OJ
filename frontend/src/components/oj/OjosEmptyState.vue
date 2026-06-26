<script setup lang="ts">
import EmptyView from '../common/EmptyView.vue'

withDefaults(
  defineProps<{
    description?: string
  }>(),
  {
    description: 'No data',
  },
)
</script>

<template>
  <EmptyView :description="description" />
</template>
