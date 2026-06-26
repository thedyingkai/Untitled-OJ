<script setup lang="ts">
import { NSpin } from 'naive-ui'

withDefaults(
  defineProps<{
    text?: string
  }>(),
  {
    text: '加载中',
  },
)
</script>

<template>
  <div class="state-view">
    <NSpin size="medium" />
    <span>{{ text }}</span>
  </div>
</template>
