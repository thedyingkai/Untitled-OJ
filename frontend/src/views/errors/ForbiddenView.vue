<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NButton, NResult } from 'naive-ui'
</script>

<template>
  <main class="error-page">
    <NResult status="403" title="403" description="当前账号没有访问权限">
      <template #footer>
        <RouterLink to="/dashboard">
          <NButton type="primary">返回工作台</NButton>
        </RouterLink>
      </template>
    </NResult>
  </main>
</template>
