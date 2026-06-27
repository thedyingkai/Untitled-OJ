<script setup lang="ts">
import { computed, h, onMounted, ref, watch } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NSpace,
  NTabPane,
  NTabs,
  NTag,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  getRuntimeOperations,
  getRuntimeService,
  getRuntimeServices,
  planRuntimeServiceRestart,
  planRuntimeServiceReload,
  planRuntimeServiceStart,
  planRuntimeServiceStop,
  reloadRuntimeServices,
} from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type {
  ModuleRuntimeService,
  RuntimeOperationItem,
  RuntimePlanItem,
  RuntimeServicesResponse,
} from '../../types/module'

const route = useRoute()
const message = useMessage()
const loading = ref(true)
const refreshing = ref(false)
const planning = ref('')
const error = ref<ApiClientError | null>(null)
const services = ref<RuntimeServicesResponse | null>(null)
const selectedService = ref<ModuleRuntimeService | null>(null)
const plan = ref<RuntimePlanItem | null>(null)
const operations = ref<RuntimeOperationItem[]>([])

const selectedServiceId = computed(() => String(route.params.serviceId ?? ''))
const allServices = computed(() => [
  ...(services.value?.services ?? []),
  ...(services.value?.workers ?? []),
])
const pageTitle = computed(() => selectedServiceId.value ? 'Runtime Service' : 'Runtime Services')
const runningCount = computed(() => allServices.value.filter((item) => item.state === 'RUNNING').length)
const blockedCount = computed(() => allServices.value.filter((item) => item.blocked_by.length > 0).length)
const planJson = computed(() => plan.value ? JSON.stringify(plan.value, null, 2) : '')
const visibleOperations = computed(() => {
  if (!selectedService.value) return operations.value
  return operations.value.filter((item) => item.service_id === selectedService.value?.service_id)
})
const ojosctlDryRun = computed(() => plan.value
  ? `ojosctl runtime apply-plan ${plan.value.plan_id}.json --dry-run`
  : '',
)
const ojosctlConfirm = computed(() => plan.value
  ? `ojosctl runtime apply-plan ${plan.value.plan_id}.json --confirm`
  : '',
)

const serviceColumns: DataTableColumns<ModuleRuntimeService> = [
  {
    title: 'Service',
    key: 'service_id',
    minWidth: 220,
    render: (row) =>
      h(
        RouterLink,
        { to: `/admin/runtime/services/${encodeURIComponent(row.service_id)}` },
        { default: () => row.service_id },
      ),
  },
  { title: 'Module', key: 'module_id', minWidth: 240 },
  { title: 'Kind', key: 'kind', width: 120 },
  { title: 'Runtime', key: 'runtime', width: 120 },
  { title: 'Lifecycle', key: 'lifecycle', width: 130 },
  { title: 'State', key: 'state', width: 130, render: (row) => stateTag(row.state) },
  { title: 'Health', key: 'health', width: 120, render: (row) => healthTag(row.health) },
  {
    title: 'Routes',
    key: 'routes',
    minWidth: 220,
    render: (row) => row.routes.join(', ') || 'none',
  },
  {
    title: 'Warnings',
    key: 'warnings',
    minWidth: 260,
    render: (row) => [...row.blocked_by, ...row.warnings].join('; ') || 'none',
  },
]

const operationColumns: DataTableColumns<RuntimeOperationItem> = [
  { title: 'Operation', key: 'operation_id', minWidth: 260 },
  { title: 'Action', key: 'action', width: 140 },
  { title: 'Status', key: 'status', width: 120, render: (row) => stateTag(row.status) },
  { title: 'Actor', key: 'actor_username', width: 140 },
  { title: 'Updated', key: 'updated_at', minWidth: 180 },
  { title: 'Error', key: 'error_message', minWidth: 220 },
]

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    services.value = await getRuntimeServices()
    operations.value = (await getRuntimeOperations()).operations
    if (selectedServiceId.value) {
      selectedService.value = (await getRuntimeService(selectedServiceId.value)).service
    } else {
      selectedService.value = null
    }
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

async function reloadRuntime(): Promise<void> {
  refreshing.value = true
  error.value = null
  try {
    await reloadRuntimeServices()
    await load(true)
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    refreshing.value = false
  }
}

async function generatePlan(action: 'start' | 'stop' | 'restart' | 'reload'): Promise<void> {
  if (!selectedService.value) return
  planning.value = action
  error.value = null
  try {
    const serviceId = selectedService.value.service_id
    if (action === 'start') {
      plan.value = (await planRuntimeServiceStart(serviceId)).plan
    } else if (action === 'stop') {
      plan.value = (await planRuntimeServiceStop(serviceId)).plan
    } else if (action === 'restart') {
      plan.value = (await planRuntimeServiceRestart(serviceId)).plan
    } else {
      plan.value = (await planRuntimeServiceReload(serviceId)).plan
    }
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    planning.value = ''
  }
}

async function copyPlan(): Promise<void> {
  if (!planJson.value) return
  await navigator.clipboard.writeText(planJson.value)
  message.success('Plan JSON copied')
}

function downloadPlan(): void {
  if (!plan.value) return
  const blob = new Blob([planJson.value], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = `${plan.value.plan_id}.json`
  anchor.click()
  URL.revokeObjectURL(url)
}

function stateTag(state: string) {
  return h(NTag, { type: stateTagType(state), size: 'small' }, { default: () => state || 'UNKNOWN' })
}

function healthTag(health: string) {
  return h(NTag, { type: healthTagType(health), size: 'small' }, { default: () => health || 'unknown' })
}

function stateTagType(state: string) {
  return state === 'RUNNING'
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
    plan.value = null
    void load(true)
  },
)

onMounted(() => void load())
</script>

<template>
  <div class="runtime-services-page">
    <OjosPageHeader
      :title="pageTitle"
      description="Plan-only service and worker lifecycle view from the Kernel Runtime driver."
      eyebrow="Hotplug L2"
    >
      <template #actions>
        <NSpace>
          <NButton secondary :loading="refreshing" @click="reloadRuntime()">Reload Runtime</NButton>
          <NButton secondary :loading="refreshing" @click="load(true)">Refresh</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="runtime-summary">
          <OjosStatCard label="Services" :value="services?.services.length ?? 0" tone="primary" />
          <OjosStatCard label="Workers" :value="services?.workers.length ?? 0" />
          <OjosStatCard label="Running" :value="runningCount" />
          <OjosStatCard label="Blocked" :value="blockedCount" tone="warning" />
        </div>

        <OjosSection v-if="selectedService" title="Service Detail">
          <NDescriptions :column="2" bordered label-placement="left">
            <NDescriptionsItem label="Service">{{ selectedService.service_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Module">{{ selectedService.module_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Kind">{{ selectedService.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="Runtime">{{ selectedService.runtime }}</NDescriptionsItem>
            <NDescriptionsItem label="Lifecycle">{{ selectedService.lifecycle }}</NDescriptionsItem>
            <NDescriptionsItem label="Compose">{{ selectedService.compose_service || 'none' }}</NDescriptionsItem>
            <NDescriptionsItem label="State">
              <NTag :type="stateTagType(selectedService.state)" size="small">
                {{ selectedService.state || 'UNKNOWN' }}
              </NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="Health">
              <NTag :type="healthTagType(selectedService.health)" size="small">
                {{ selectedService.health || 'unknown' }}
              </NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="Routes">{{ selectedService.routes.join(', ') || 'none' }}</NDescriptionsItem>
            <NDescriptionsItem label="Warnings">{{ [...selectedService.blocked_by, ...selectedService.warnings].join('; ') || 'none' }}</NDescriptionsItem>
          </NDescriptions>
          <NSpace class="plan-actions">
            <NButton secondary :loading="planning === 'start'" @click="generatePlan('start')">Plan Start</NButton>
            <NButton secondary :loading="planning === 'stop'" @click="generatePlan('stop')">Plan Stop</NButton>
            <NButton secondary :loading="planning === 'restart'" @click="generatePlan('restart')">Plan Restart</NButton>
            <NButton secondary :loading="planning === 'reload'" @click="generatePlan('reload')">Plan Reload</NButton>
          </NSpace>
          <OjosSection v-if="plan" title="Runtime Plan">
            <NSpace class="plan-actions">
              <NButton secondary @click="copyPlan()">Copy JSON</NButton>
              <NButton secondary @click="downloadPlan()">Download JSON</NButton>
            </NSpace>
            <NDescriptions :column="2" bordered label-placement="left" class="plan-meta">
              <NDescriptionsItem label="Operation">{{ plan.operation_id }}</NDescriptionsItem>
              <NDescriptionsItem label="Expires">{{ plan.expires_at }}</NDescriptionsItem>
              <NDescriptionsItem label="Operator Apply">{{ plan.can_apply ? 'allowed' : 'blocked' }}</NDescriptionsItem>
              <NDescriptionsItem label="Gateway Apply">{{ plan.apply_enabled ? 'enabled' : 'disabled' }}</NDescriptionsItem>
              <NDescriptionsItem label="Dry Run">{{ ojosctlDryRun }}</NDescriptionsItem>
              <NDescriptionsItem label="Confirm">{{ ojosctlConfirm }}</NDescriptionsItem>
            </NDescriptions>
            <OjosJsonViewer :value="plan" />
          </OjosSection>
          <OjosSection title="Operation History">
            <EmptyView v-if="visibleOperations.length === 0" description="No runtime operations" />
            <NDataTable v-else :columns="operationColumns" :data="visibleOperations" :pagination="{ pageSize: 8 }" :bordered="false" />
          </OjosSection>
        </OjosSection>

        <OjosSection title="Runtime Inventory">
          <NTabs type="line" animated>
            <NTabPane name="services" tab="Services">
              <EmptyView v-if="!services?.services.length" description="No runtime services" />
              <NDataTable v-else :columns="serviceColumns" :data="services.services" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="workers" tab="Workers">
              <EmptyView v-if="!services?.workers.length" description="No runtime workers" />
              <NDataTable v-else :columns="serviceColumns" :data="services.workers" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.runtime-services-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.runtime-summary {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.plan-actions {
  margin-top: 14px;
}

.plan-meta {
  margin: 12px 0;
}

@media (max-width: 960px) {
  .runtime-summary {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .runtime-summary {
    grid-template-columns: 1fr;
  }
}
</style>
