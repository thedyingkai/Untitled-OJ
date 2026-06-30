<script setup lang="ts">
import PermissionGuard from '../common/PermissionGuard.vue'

defineProps<{
  roles?: string[]
  permissions?: string[]
}>()
</script>

<template>
  <PermissionGuard :roles="roles" :permissions="permissions">
    <slot />
  </PermissionGuard>
</template>
