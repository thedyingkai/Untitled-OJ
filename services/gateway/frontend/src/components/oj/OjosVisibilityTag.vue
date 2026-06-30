<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

import { getProblemVisibilityMeta } from '../../utils/status'

const props = defineProps<{
  visibility: string
}>()

const meta = computed(() => getProblemVisibilityMeta(props.visibility))
</script>

<template>
  <NTag :type="meta.type" size="small" :class="['ojos-tag', meta.className]" :title="meta.description">
    {{ meta.label }}
  </NTag>
</template>
