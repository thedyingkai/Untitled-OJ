<script setup lang="ts">
import { computed } from 'vue'
import { NInput } from 'naive-ui'

const props = withDefaults(
  defineProps<{
    modelValue: string
    language?: string
    readonly?: boolean
    placeholder?: string
    maxLength?: number
  }>(),
  {
    language: 'text',
    readonly: false,
    placeholder: '',
    maxLength: 200000,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const value = computed({
  get: () => props.modelValue,
  set: (next: string) => emit('update:modelValue', next),
})
</script>

<template>
  <div class="code-editor" :data-language="language">
    <NInput
      v-model:value="value"
      type="textarea"
      :readonly="readonly"
      :placeholder="placeholder"
      :maxlength="maxLength"
      show-count
      :autosize="{ minRows: 14, maxRows: 30 }"
    />
  </div>
</template>
