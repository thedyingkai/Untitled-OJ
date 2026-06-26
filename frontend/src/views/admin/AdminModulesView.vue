<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import {
  NButton,
  NDataTable,
  NInput,
  NSelect,
  NSpace,
  NTag,
  type DataTableColumns,
} from 'naive-ui'

import { listModules, listModuleSets } from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import type { ModuleNodeItem, ModuleSetItem } from '../../types/module'

const modules = ref<ModuleNodeItem[]>([])
const sets = ref<ModuleSetItem[]>([])
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const keyword = ref('')
const setFilter = ref<string | null>(null)
const statusFilter = ref<string | null>(null)

const setOptions = computed(() => [
  { label: '全部集合', value: '' },
  ...sets.value.map((item) => ({ label: `${item.name} (${item.set_id})`, value: item.set_id })),
])

const statusOptions = computed(() => {
  const values = Array.from(new Set(modules.value.map((item) => item.status))).sort()
  return [{ label: '全部状态', value: '' }, ...values.map((value) => ({ label: value, value }))]
})

const filteredModules = computed(() => {
  const term = keyword.value.trim().toLowerCase()
  return modules.value.filter((item) => {
    const matchesKeyword =
      !term ||
      item.module_id.toLowerCase().includes(term) ||
      item.name.toLowerCase().includes(term) ||
      item.description.toLowerCase().includes(term)
    const matchesSet = !setFilter.value || item.set_id === setFilter.value
    const matchesStatus = !statusFilter.value || item.status === statusFilter.value
    return matchesKeyword && matchesSet && matchesStatus
  })
})

const columns = computed<DataTableColumns<ModuleNodeItem>>(() => [
  {
    title: 'module_id',
    key: 'module_id',
    render: (row) =>
      h(
        RouterLink,
        { to: `/admin/modules/${encodeURIComponent(row.module_id)}` },
        { default: () => row.module_id },
      ),
  },
  { title: 'name', key: 'name' },
  { title: 'set_id', key: 'set_id' },
  { title: 'version', key: 'version', width: 100 },
  { title: 'status', key: 'status', width: 120, render: (row) => hStatus(row.status) },
  { title: 'kind', key: 'kind', width: 120 },
  { title: 'description', key: 'description' },
])

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    const [moduleResp, setResp] = await Promise.all([listModules(), listModuleSets()])
    modules.value = moduleResp.modules
    sets.value = setResp.sets
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function hStatus(status: string) {
  const type = status === 'ENABLED' ? 'success' : status.includes('FAILED') ? 'error' : 'default'
  return h(NTag, { type, size: 'small', round: true }, { default: () => status })
}

onMounted(() => void load())
</script>

<template>
  <PageCard title="模块注册表">
    <template #headerExtra>
      <NSpace>
        <RouterLink to="/admin/modules/topology">拓扑视图</RouterLink>
        <NButton size="small" secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </NSpace>
    </template>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NSpace v-else vertical size="large">
        <NSpace>
          <NInput v-model:value="keyword" clearable placeholder="搜索 module_id / name / description" />
          <NSelect
            v-model:value="setFilter"
            :options="setOptions"
            clearable
            placeholder="按集合筛选"
            style="width: 240px"
          />
          <NSelect
            v-model:value="statusFilter"
            :options="statusOptions"
            clearable
            placeholder="按状态筛选"
            style="width: 180px"
          />
        </NSpace>

        <EmptyView v-if="filteredModules.length === 0" description="没有匹配的模块" />
        <NDataTable
          v-else
          :columns="columns"
          :data="filteredModules"
          :pagination="{ pageSize: 12 }"
        />
      </NSpace>
    </template>
  </PageCard>
</template>
