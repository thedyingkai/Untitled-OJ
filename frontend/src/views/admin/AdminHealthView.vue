<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref } from 'vue'
import { NButton, NDataTable, NGrid, NGridItem, NSpace, NSwitch, NText, type DataTableColumns } from 'naive-ui'

import { getAdminHealth } from '../../api/health'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import StatusTag from '../../components/common/StatusTag.vue'
import type { AdminHealthResponse, HealthComponent } from '../../types/health'

const health = ref<AdminHealthResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const autoRefresh = ref(true)
const error = ref<ApiClientError | null>(null)
let timer: number | undefined

const columns = computed<DataTableColumns<HealthComponent>>(() => [
  { title: 'Component', key: 'name' },
  { title: 'Status', key: 'status', render: (row) => h(StatusTag, { status: row.status }) },
  { title: 'Latency', key: 'latency_ms', render: (row) => `${row.latency_ms} ms` },
  { title: 'Message', key: 'message' },
])

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    health.value = await getAdminHealth()
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function startTimer(): void {
  stopTimer()
  timer = window.setInterval(() => {
    if (autoRefresh.value) {
      void load(true)
    }
  }, 5000)
}

function stopTimer(): void {
  if (timer) {
    window.clearInterval(timer)
    timer = undefined
  }
}

onMounted(() => {
  void load()
  startTimer()
})

onBeforeUnmount(stopTimer)
</script>

<template>
  <PageCard title="Service Health">
    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NSpace v-else vertical size="large">
        <NSpace justify="space-between" align="center">
          <StatusTag :status="health?.status || 'unknown'" />
          <NSpace align="center">
            <NText depth="3">Auto refresh</NText>
            <NSwitch v-model:value="autoRefresh" />
            <NButton :loading="refreshing" secondary @click="load(true)">Refresh</NButton>
          </NSpace>
        </NSpace>

        <NGrid :cols="3" :x-gap="12" :y-gap="12" responsive="screen">
          <NGridItem class="metric">
            <strong>{{ health?.worker_online_count ?? 0 }}</strong>
            <span>Workers Online</span>
          </NGridItem>
          <NGridItem class="metric">
            <strong>{{ health?.queue_pending ?? 0 }}</strong>
            <span>Queue Pending</span>
          </NGridItem>
          <NGridItem class="metric">
            <strong>{{ health?.internal_auth }}</strong>
            <span>Internal Auth</span>
          </NGridItem>
        </NGrid>

        <EmptyView
          v-if="(health?.components || []).length === 0"
          description="No health components reported"
        />
        <NDataTable v-else :columns="columns" :data="health?.components || []" />
      </NSpace>
    </template>
  </PageCard>
</template>

<style scoped>
.metric {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  padding: 14px;
  background: #fff;
}

.metric strong {
  display: block;
  font-size: 24px;
  line-height: 1.2;
}

.metric span {
  color: #667085;
  font-size: 13px;
}
</style>
