<script setup lang="ts">
import CopyButton from '../common/CopyButton.vue'

withDefaults(
  defineProps<{
    code?: string
    label?: string
  }>(),
  {
    code: '',
    label: '',
  },
)
</script>

<template>
  <div class="ojos-code-block">
    <div v-if="label || code" class="ojos-code-header">
      <span>{{ label }}</span>
      <CopyButton v-if="code" :text="code" />
    </div>
    <pre>{{ code || '-' }}</pre>
  </div>
</template>
