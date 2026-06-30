<script setup lang="ts">
import { computed } from 'vue'

import { useAuthStore } from '../../stores/auth'

const props = withDefaults(
  defineProps<{
    roles?: string[]
    permissions?: string[]
    requireAll?: boolean
  }>(),
  {
    roles: () => [],
    permissions: () => [],
    requireAll: false,
  },
)

const auth = useAuthStore()

const allowed = computed(() => {
  const roleOk =
    props.roles.length === 0 ||
    (props.requireAll
      ? props.roles.every((role) => auth.roles.includes(role))
      : props.roles.some((role) => auth.roles.includes(role)))

  const permissionOk =
    props.permissions.length === 0 ||
    (props.requireAll
      ? props.permissions.every((permission) => auth.permissions.includes(permission))
      : props.permissions.some((permission) => auth.permissions.includes(permission)))

  return roleOk && permissionOk
})
</script>

<template>
  <slot v-if="allowed" />
  <slot v-else name="fallback" />
</template>
