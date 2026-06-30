<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NButton, NResult } from 'naive-ui'
</script>

<template>
  <main class="error-page">
    <NResult status="404" title="Page not found" description="The requested route does not exist.">
      <template #footer>
        <RouterLink to="/dashboard">
          <NButton type="primary">Back to dashboard</NButton>
        </RouterLink>
      </template>
    </NResult>
  </main>
</template>
