<script setup lang="ts">
import { NEmpty } from 'naive-ui'

withDefaults(
  defineProps<{
    description?: string
  }>(),
  {
    description: '暂无数据',
  },
)
</script>

<template>
  <div class="state-view">
    <NEmpty :description="description">
      <template v-if="$slots.default" #extra>
        <slot />
      </template>
    </NEmpty>
  </div>
</template>
