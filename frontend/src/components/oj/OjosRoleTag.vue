<script setup lang="ts">
import { computed } from 'vue'
import { NTag } from 'naive-ui'

const props = withDefaults(
  defineProps<{
    role: string
    closable?: boolean
  }>(),
  {
    closable: false,
  },
)

const emit = defineEmits<{
  close: []
}>()

const className = computed(() => {
  if (props.role === 'super_admin' || props.role === 'admin') return 'role-admin'
  if (props.role.startsWith('problem_')) return 'role-problem'
  return 'role-default'
})
</script>

<template>
  <NTag size="small" :closable="closable" :class="['ojos-tag', className]" @close="emit('close')">
    {{ role }}
  </NTag>
</template>
