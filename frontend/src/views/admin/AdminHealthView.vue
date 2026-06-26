<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref } from 'vue'
import { NButton, NDataTable, NSpace, NSwitch, NText, type DataTableColumns } from 'naive-ui'

import { getAdminHealth } from '../../api/health'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosHealthBadge from '../../components/oj/OjosHealthBadge.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type { AdminHealthResponse, HealthComponent } from '../../types/health'
import { formatDateTime, formatDuration } from '../../utils/format'

const health = ref<AdminHealthResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const autoRefresh = ref(true)
const error = ref<ApiClientError | null>(null)
const lastUpdated = ref<string>('')
let timer: number | undefined

const columns = computed<DataTableColumns<HealthComponent>>(() => [
  { title: 'Component', key: 'name', minWidth: 170 },
  {
    title: 'Status',
    key: 'status',
    width: 130,
    render: (row) => h(OjosHealthBadge, { status: row.status }),
  },
  { title: 'Latency', key: 'latency_ms', width: 120, render: (row) => formatDuration(row.latency_ms) },
  { title: 'Message', key: 'message', render: (row) => row.message || '-' },
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
    lastUpdated.value = new Date().toISOString()
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
  <div class="admin-health-page">
    <OjosPageHeader
      title="Service Health"
      description="Runtime health for gateway, APIs, PostgreSQL, Redis, storage, workers, and queue."
      eyebrow="Admin"
    >
      <template #actions>
        <NSpace align="center">
          <NText depth="3">Auto refresh</NText>
          <NSwitch v-model:value="autoRefresh" />
          <NButton :loading="refreshing" secondary @click="load(true)">Refresh</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="health-summary-grid">
          <div class="health-state-card">
            <span>Overall status</span>
            <OjosHealthBadge :status="health?.status || 'unknown'" />
            <small>Last updated {{ formatDateTime(lastUpdated) }}</small>
          </div>
          <OjosStatCard
            label="Workers Online"
            :value="health?.worker_online_count ?? 0"
            tone="success"
          />
          <OjosStatCard label="Queue Pending" :value="health?.queue_pending ?? 0" tone="primary" />
          <OjosStatCard label="Internal Auth" :value="health?.internal_auth || '-'" />
        </div>

        <OjosSection title="Components" description="Each row is reported by the real Admin Health API.">
          <EmptyView
            v-if="(health?.components || []).length === 0"
            description="No health components reported"
          />
          <NDataTable v-else :columns="columns" :data="health?.components || []" :bordered="false" />
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.admin-health-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.health-summary-grid {
  display: grid;
  grid-template-columns: 1.4fr repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.health-state-card {
  display: flex;
  min-height: 92px;
  flex-direction: column;
  justify-content: center;
  gap: 8px;
  border: 1px solid var(--border-soft);
  border-radius: var(--ojos-radius);
  padding: 14px 16px;
  background: var(--surface);
  box-shadow: var(--shadow-sm);
}

.health-state-card span,
.health-state-card small {
  color: var(--muted);
}

@media (max-width: 1000px) {
  .health-summary-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
