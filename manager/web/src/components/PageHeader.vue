<script setup lang="ts">
defineProps<{ title: string; subtitle?: string }>();
</script>

<template>
  <header class="page-header">
    <div>
      <h1>{{ title }}</h1>
      <p v-if="subtitle" class="subtitle">{{ subtitle }}</p>
    </div>
    <div class="actions"><slot /></div>
  </header>
</template>

<style scoped>
.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 16px 22px;
  border-bottom: 1px solid var(--border);
  background: var(--bg-soft);
  flex-shrink: 0;
}
h1 {
  font-size: 16px;
  font-weight: 600;
}
.subtitle {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--faint);
}
.actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
