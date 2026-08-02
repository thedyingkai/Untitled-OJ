<script setup lang="ts">
defineProps<{ open: boolean; title: string; width?: string }>();
const emit = defineEmits<{ close: [] }>();
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="overlay" @mousedown.self="emit('close')">
        <div class="modal fade-in" :style="{ width: width || '440px' }">
          <div class="modal-head">
            <h3>{{ title }}</h3>
            <button class="btn ghost sm" @click="emit('close')">✕</button>
          </div>
          <div class="modal-body"><slot /></div>
          <div class="modal-foot" v-if="$slots.footer">
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  background: rgba(5, 8, 14, 0.66);
  backdrop-filter: blur(3px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
}
.modal {
  max-width: calc(100vw - 40px);
  max-height: calc(100vh - 60px);
  display: flex;
  flex-direction: column;
  background: var(--panel);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow);
}
.modal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 15px 18px 12px;
  border-bottom: 1px solid var(--border);
}
.modal-head h3 {
  font-size: 14px;
}
.modal-body {
  padding: 16px 18px;
  overflow-y: auto;
}
.modal-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 18px 15px;
  border-top: 1px solid var(--border);
}
.modal-enter-active,
.modal-leave-active {
  transition: opacity 0.15s ease;
}
.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}
</style>
