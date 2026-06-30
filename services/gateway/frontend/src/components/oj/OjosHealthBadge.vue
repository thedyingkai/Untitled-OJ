<script setup lang="ts">
import OjosStatusTag from './OjosStatusTag.vue'

defineProps<{
  status: string
}>()
</script>

<template>
  <OjosStatusTag :status="status" domain="health" />
</template>
