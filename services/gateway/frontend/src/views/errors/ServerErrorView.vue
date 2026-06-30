<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NButton, NResult } from 'naive-ui'
</script>

<template>
  <main class="error-page">
    <NResult
      status="500"
      title="Service unavailable"
      description="The application could not complete the request."
    >
      <template #footer>
        <RouterLink to="/dashboard">
          <NButton type="primary">Back to dashboard</NButton>
        </RouterLink>
      </template>
    </NResult>
  </main>
</template>
