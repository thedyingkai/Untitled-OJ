<script setup lang="ts">
import { computed } from 'vue'
import { NAlert, NText } from 'naive-ui'

import { toApiClientError } from '../../api/client'

const props = withDefaults(
  defineProps<{
    error?: unknown
    title?: string
  }>(),
  {
    error: undefined,
    title: '请求失败',
  },
)

const normalized = computed(() => (props.error ? toApiClientError(props.error) : null))
</script>

<template>
  <NAlert v-if="normalized" type="error" :title="title" class="api-error-alert">
    <div>{{ normalized.message }}</div>
    <NText v-if="normalized.requestId" depth="3">
      request_id: {{ normalized.requestId }}
    </NText>
  </NAlert>
</template>
