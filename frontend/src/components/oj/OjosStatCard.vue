<script setup lang="ts">
withDefaults(
  defineProps<{
    label: string
    value: string | number
    note?: string
    tone?: 'default' | 'primary' | 'success' | 'warning' | 'danger'
  }>(),
  {
    note: '',
    tone: 'default',
  },
)
</script>

<template>
  <div :class="['ojos-stat-card', `tone-${tone}`]">
    <span>{{ label }}</span>
    <strong>{{ value }}</strong>
    <small v-if="note">{{ note }}</small>
  </div>
</template>
