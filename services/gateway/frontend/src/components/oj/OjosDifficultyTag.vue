<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

import { getDifficultyMeta } from '../../utils/status'

const props = withDefaults(
  defineProps<{
    difficulty: string
    size?: 'small' | 'medium' | 'large'
  }>(),
  {
    size: 'small',
  },
)

const meta = computed(() => getDifficultyMeta(props.difficulty))
</script>

<template>
  <NTag
    :type="meta.type"
    :size="size"
    :class="['ojos-tag', meta.className]"
    :title="meta.description"
  >
    {{ meta.label }}
  </NTag>
</template>
