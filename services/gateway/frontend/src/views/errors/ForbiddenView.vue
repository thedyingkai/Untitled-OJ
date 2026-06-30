<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NButton, NResult } from 'naive-ui'
</script>

<template>
  <main class="error-page">
    <NResult
      status="403"
      title="Access denied"
      description="Your account does not have permission to open this page."
    >
      <template #footer>
        <RouterLink to="/dashboard">
          <NButton type="primary">Back to dashboard</NButton>
        </RouterLink>
      </template>
    </NResult>
  </main>
</template>
