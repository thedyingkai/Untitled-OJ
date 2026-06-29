<script setup lang="ts">
import { computed, h, onMounted, ref, watch } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NTag,
  type DataTableColumns,
} from 'naive-ui'

import {
  getServiceStatusItem,
  getServiceStatusList,
  getServiceStatusOperations,
} from '../../api/services'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type {
  ServiceStatusItem,
  ServiceStatusListResponse,
  ServiceStatusOperationItem,
} from '../../types/service'

const route = useRoute()
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const services = ref<ServiceStatusListResponse | null>(null)
const selectedService = ref<ServiceStatusItem | null>(null)
const operations = ref<ServiceStatusOperationItem[]>([])

const selectedServiceId = computed(() => String(route.params.serviceId ?? ''))
const allServices = computed(() => [
  ...(services.value?.services ?? []),
  ...(services.value?.workers ?? []),
])
const pageTitle = computed(() => (selectedServiceId.value ? 'Service 状态详情' : 'Service 状态'))
const runningCount = computed(() => allServices.value.filter((item) => item.state === 'RUNNING').length)
const blockedCount = computed(() => allServices.value.filter((item) => item.blocked_by.length > 0).length)
const visibleOperations = computed(() => {
  if (!selectedService.value) return operations.value
  return operations.value.filter((item) => item.service_id === selectedService.value?.service_id)
})

const serviceColumns: DataTableColumns<ServiceStatusItem> = [
  {
    title: 'Service',
    key: 'service_id',
    minWidth: 220,
    render: (row) =>
      h(
        RouterLink,
        { to: `/admin/services/status/${encodeURIComponent(row.service_id)}` },
        { default: () => row.service_id },
      ),
  },
  { title: 'Owner', key: 'owner_service_id', minWidth: 220 },
  { title: '类型', key: 'kind', width: 120 },
  { title: 'Runtime', key: 'runtime', width: 120 },
  { title: '生命周期', key: 'lifecycle', width: 130 },
  { title: '状态', key: 'state', width: 130, render: (row) => stateTag(row.state) },
  { title: '健康', key: 'health', width: 120, render: (row) => healthTag(row.health) },
  { title: '路由', key: 'routes', minWidth: 220, render: (row) => row.routes.join(', ') || '无' },
  {
    title: '告警',
    key: 'warnings',
    minWidth: 260,
    render: (row) => [...row.blocked_by, ...row.warnings].join('; ') || '无',
  },
]

const operationColumns: DataTableColumns<ServiceStatusOperationItem> = [
  { title: '操作', key: 'operation_id', minWidth: 260 },
  { title: '动作', key: 'action', width: 140 },
  { title: '状态', key: 'status', width: 120, render: (row) => stateTag(row.status) },
  { title: '操作者', key: 'actor_username', width: 140 },
  { title: '更新时间', key: 'updated_at', minWidth: 180 },
  { title: '错误', key: 'error_message', minWidth: 220 },
]

async function load(silent = false): Promise<void> {
  refreshing.value = silent
  loading.value = !silent
  error.value = null
  try {
    services.value = await getServiceStatusList()
    operations.value = (await getServiceStatusOperations()).operations
    selectedService.value = selectedServiceId.value
      ? (await getServiceStatusItem(selectedServiceId.value)).service
      : null
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function stateTag(state: string) {
  return h(NTag, { type: stateTagType(state), size: 'small' }, { default: () => state || 'UNKNOWN' })
}

function healthTag(health: string) {
  return h(NTag, { type: healthTagType(health), size: 'small' }, { default: () => health || 'unknown' })
}

function stateTagType(state: string) {
  return state === 'RUNNING' || state === 'SUCCEEDED'
    ? 'success'
    : state === 'FAILED' || state === 'STOPPED'
      ? 'error'
      : state === 'DEGRADED'
        ? 'warning'
        : 'default'
}

function healthTagType(health: string) {
  return health === 'ok'
    ? 'success'
    : health === 'error'
      ? 'error'
      : health === 'degraded'
        ? 'warning'
        : 'default'
}

watch(
  () => route.params.serviceId,
  () => {
    void load(true)
  },
)

onMounted(() => void load())
</script>

<template>
  <div class="service-status-page">
    <OjosPageHeader
      :title="pageTitle"
      description="Web Shell 只读展示 Service 状态、健康和操作记录。Service 启停、连接和拓扑变更由 OJOS Orchestrator GUI/TUI 处理。"
      eyebrow="只读 Service 状态"
    />

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="service-status-summary">
          <OjosStatCard label="Service" :value="services?.services.length ?? 0" tone="primary" />
          <OjosStatCard label="Worker" :value="services?.workers.length ?? 0" />
          <OjosStatCard label="运行中" :value="runningCount" />
          <OjosStatCard label="受阻" :value="blockedCount" tone="warning" />
        </div>

        <OjosSection v-if="selectedService" title="Service 详情">
          <NDescriptions :column="2" bordered label-placement="left">
            <NDescriptionsItem label="Service">{{ selectedService.service_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Owner">{{ selectedService.owner_service_id }}</NDescriptionsItem>
            <NDescriptionsItem label="类型">{{ selectedService.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="Runtime">{{ selectedService.runtime }}</NDescriptionsItem>
            <NDescriptionsItem label="生命周期">{{ selectedService.lifecycle }}</NDescriptionsItem>
            <NDescriptionsItem label="Compose">{{ selectedService.compose_service || '无' }}</NDescriptionsItem>
            <NDescriptionsItem label="状态">
              <NTag :type="stateTagType(selectedService.state)" size="small">
                {{ selectedService.state || 'UNKNOWN' }}
              </NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="健康">
              <NTag :type="healthTagType(selectedService.health)" size="small">
                {{ selectedService.health || 'unknown' }}
              </NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="路由">{{ selectedService.routes.join(', ') || '无' }}</NDescriptionsItem>
            <NDescriptionsItem label="告警">{{ [...selectedService.blocked_by, ...selectedService.warnings].join('; ') || '无' }}</NDescriptionsItem>
          </NDescriptions>

          <OjosSection title="操作记录">
            <EmptyView v-if="visibleOperations.length === 0" description="暂无 Service 操作记录" />
            <NDataTable v-else :columns="operationColumns" :data="visibleOperations" :pagination="{ pageSize: 8 }" :bordered="false" />
          </OjosSection>
        </OjosSection>

        <OjosSection title="Service 状态清单">
          <OjosSection title="Service">
            <EmptyView v-if="!services?.services.length" description="暂无 Service 状态" />
            <NDataTable v-else :columns="serviceColumns" :data="services.services" :pagination="{ pageSize: 12 }" :bordered="false" />
          </OjosSection>
          <OjosSection title="Worker">
            <EmptyView v-if="!services?.workers.length" description="暂无 Worker 状态" />
            <NDataTable v-else :columns="serviceColumns" :data="services.workers" :pagination="{ pageSize: 12 }" :bordered="false" />
          </OjosSection>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.service-status-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.service-status-summary {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

@media (max-width: 900px) {
  .service-status-summary {
    grid-template-columns: 1fr;
  }
}
</style>
