<script setup lang="ts">
import { NCard } from 'naive-ui'

withDefaults(
  defineProps<{
    title?: string
    bordered?: boolean
  }>(),
  {
    title: '',
    bordered: true,
  },
)
</script>

<template>
  <NCard class="page-card" :title="title || undefined" :bordered="bordered">
    <template v-if="$slots.headerExtra" #header-extra>
      <slot name="headerExtra" />
    </template>
    <slot />
  </NCard>
</template>
