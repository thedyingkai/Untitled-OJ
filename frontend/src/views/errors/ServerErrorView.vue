<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NButton, NResult } from 'naive-ui'
</script>

<template>
  <main class="error-page">
    <NResult status="500" title="500" description="服务暂时不可用">
      <template #footer>
        <RouterLink to="/dashboard">
          <NButton type="primary">返回工作台</NButton>
        </RouterLink>
      </template>
    </NResult>
  </main>
</template>
