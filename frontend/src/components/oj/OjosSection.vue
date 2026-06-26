<script setup lang="ts">
withDefaults(
  defineProps<{
    title?: string
    description?: string
  }>(),
  {
    title: '',
    description: '',
  },
)
</script>

<template>
  <section class="ojos-section">
    <header v-if="title || description || $slots.extra" class="ojos-section-header">
      <div>
        <h2 v-if="title">{{ title }}</h2>
        <p v-if="description">{{ description }}</p>
      </div>
      <div v-if="$slots.extra" class="ojos-section-extra">
        <slot name="extra" />
      </div>
    </header>
    <slot />
  </section>
</template>
