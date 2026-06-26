<script setup lang="ts">
import { NButton, useDialog } from 'naive-ui'

const props = withDefaults(
  defineProps<{
    label: string
    title?: string
    content?: string
    type?: 'default' | 'primary' | 'info' | 'success' | 'warning' | 'error'
    disabled?: boolean
  }>(),
  {
    title: 'Confirm action',
    content: 'This operation will take effect immediately.',
    type: 'default',
    disabled: false,
  },
)

const emit = defineEmits<{
  confirm: []
}>()

const dialog = useDialog()

function openConfirm(): void {
  dialog.warning({
    title: props.title,
    content: props.content,
    positiveText: 'Confirm',
    negativeText: 'Cancel',
    onPositiveClick: () => {
      emit('confirm')
    },
  })
}
</script>

<template>
  <NButton :type="type" :disabled="disabled" @click="openConfirm">
    {{ label }}
  </NButton>
</template>
