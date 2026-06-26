<template>
  <div class="ojos-toolbar">
    <div class="ojos-toolbar-main">
      <slot />
    </div>
    <div v-if="$slots.actions" class="ojos-toolbar-actions">
      <slot name="actions" />
    </div>
  </div>
</template>
