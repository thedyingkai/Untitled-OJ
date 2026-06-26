<script setup lang="ts">
import JsonViewer from '../common/JsonViewer.vue'

defineProps<{
  value: unknown
}>()
</script>

<template>
  <JsonViewer :value="value" />
</template>
