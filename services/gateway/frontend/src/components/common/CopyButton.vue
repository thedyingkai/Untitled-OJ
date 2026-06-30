<script setup lang="ts">
import { NButton, useMessage } from 'naive-ui'

const props = withDefaults(
  defineProps<{
    text: string
    label?: string
    size?: 'tiny' | 'small' | 'medium' | 'large'
  }>(),
  {
    label: '复制',
    size: 'small',
  },
)

const message = useMessage()

async function copy(): Promise<void> {
  await navigator.clipboard.writeText(props.text)
  message.success('已复制')
}
</script>

<template>
  <NButton :size="size" secondary @click="copy">
    {{ label }}
  </NButton>
</template>
