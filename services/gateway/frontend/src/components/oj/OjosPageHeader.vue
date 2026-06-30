<script setup lang="ts">
withDefaults(
  defineProps<{
    title: string
    description?: string
    eyebrow?: string
  }>(),
  {
    description: '',
    eyebrow: '',
  },
)
</script>

<template>
  <header class="ojos-page-header">
    <div>
      <span v-if="eyebrow" class="ojos-page-eyebrow">{{ eyebrow }}</span>
      <h1>{{ title }}</h1>
      <p v-if="description">{{ description }}</p>
    </div>
    <div v-if="$slots.actions" class="ojos-page-actions">
      <slot name="actions" />
    </div>
  </header>
</template>
