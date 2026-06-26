<script setup lang="ts">
import ApiErrorAlert from '../common/ApiErrorAlert.vue'

defineProps<{
  error: unknown
  title?: string
}>()
</script>

<template>
  <ApiErrorAlert :error="error" :title="title" />
</template>
