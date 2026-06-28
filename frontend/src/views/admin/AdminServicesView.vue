<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NInput, NSelect, NSpace, type DataTableColumns } from 'naive-ui'

import { listServiceSets, listServices } from '../../api/services'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosServiceStatusTag from '../../components/oj/OjosServiceStatusTag.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type { ServiceNodeItem, ServiceSetItem } from '../../types/service'

const services = ref<ServiceNodeItem[]>([])
const sets = ref<ServiceSetItem[]>([])
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const keyword = ref('')
const setFilter = ref<string | null>(null)
const statusFilter = ref<string | null>(null)

const setOptions = computed(() => [
  { label: '全部 Set', value: '' },
  ...sets.value.map((item) => ({ label: `${item.name} (${item.set_id})`, value: item.set_id })),
])

const statusOptions = computed(() => {
  const values = Array.from(new Set(services.value.map((item) => item.status))).sort()
  return [{ label: '全部状态', value: '' }, ...values.map((value) => ({ label: value, value }))]
})

const filteredServices = computed(() => {
  const term = keyword.value.trim().toLowerCase()
  return services.value.filter((item) => {
    const matchesKeyword =
      !term ||
      item.service_id.toLowerCase().includes(term) ||
      item.name.toLowerCase().includes(term) ||
      item.description.toLowerCase().includes(term)
    const matchesSet = !setFilter.value || item.set_id === setFilter.value
    const matchesStatus = !statusFilter.value || item.status === statusFilter.value
    return matchesKeyword && matchesSet && matchesStatus
  })
})

const columns = computed<DataTableColumns<ServiceNodeItem>>(() => [
  {
    title: 'Service',
    key: 'service_id',
    width: 260,
    render: (row) =>
      h(
        RouterLink,
        { to: `/admin/services/${encodeURIComponent(row.service_id)}`, class: 'table-link' },
        { default: () => row.service_id },
      ),
  },
  { title: '名称', key: 'name', width: 180 },
  { title: 'Set', key: 'set_id', width: 160 },
  { title: '版本', key: 'version', width: 110 },
  { title: '状态', key: 'status', width: 120, render: (row) => h(OjosServiceStatusTag, { status: row.status }) },
  { title: '类型', key: 'kind', width: 140 },
  { title: '说明', key: 'description' },
])

async function load(silent = false): Promise<void> {
  refreshing.value = silent
  loading.value = !silent
  error.value = null
  try {
    const [serviceResp, setResp] = await Promise.all([listServices(), listServiceSets()])
    services.value = serviceResp.services
    sets.value = setResp.sets
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

onMounted(() => void load())
</script>

<template>
  <div class="services-page">
    <OjosPageHeader
      title="Service 状态"
      description="Web Shell 仅展示 Root Runtime 生成的 Service 注册表和运行状态；安装、删除、热插拔与 Link 配置由 Root Installer / Runtime Manager 完成。"
      eyebrow="只读视图"
    >
      <template #actions>
        <RouterLink to="/admin/topology">
          <NButton secondary>Topology</NButton>
        </RouterLink>
        <NButton secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="service-summary">
          <OjosStatCard label="Services" :value="services.length" tone="primary" />
          <OjosStatCard label="Sets" :value="sets.length" />
          <OjosStatCard label="当前可见" :value="filteredServices.length" />
        </div>

        <OjosSection title="Service Registry">
          <NSpace class="service-filters">
            <NInput v-model:value="keyword" clearable placeholder="搜索 Service ID、名称或说明" style="min-width: 280px" />
            <NSelect v-model:value="setFilter" :options="setOptions" clearable placeholder="按 Set 过滤" style="width: 240px" />
            <NSelect v-model:value="statusFilter" :options="statusOptions" clearable placeholder="按状态过滤" style="width: 180px" />
          </NSpace>

          <EmptyView v-if="filteredServices.length === 0" description="没有匹配的 Service" />
          <NDataTable
            v-else
            :columns="columns"
            :data="filteredServices"
            :pagination="{ pageSize: 12 }"
            :bordered="false"
          />
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.services-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.service-summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.service-filters {
  width: 100%;
  margin-bottom: 12px;
}

@media (max-width: 900px) {
  .service-summary {
    grid-template-columns: 1fr;
  }
}
</style>
