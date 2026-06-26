<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NInput, NSelect, NSpace, type DataTableColumns } from 'naive-ui'

import { listModuleSets, listModules } from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosModuleStatusTag from '../../components/oj/OjosModuleStatusTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
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
  { label: 'All sets', value: '' },
  ...sets.value.map((item) => ({ label: `${item.name} (${item.set_id})`, value: item.set_id })),
])

const statusOptions = computed(() => {
  const values = Array.from(new Set(modules.value.map((item) => item.status))).sort()
  return [{ label: 'All statuses', value: '' }, ...values.map((value) => ({ label: value, value }))]
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
    title: 'Module',
    key: 'module_id',
    width: 260,
    render: (row) =>
      h(
        RouterLink,
        {
          to: `/admin/modules/${encodeURIComponent(row.module_id)}`,
          class: 'table-link',
        },
        { default: () => row.module_id },
      ),
  },
  { title: 'Name', key: 'name', width: 180 },
  { title: 'Set', key: 'set_id', width: 160 },
  { title: 'Version', key: 'version', width: 110 },
  { title: 'Status', key: 'status', width: 120, render: (row) => h(OjosModuleStatusTag, { status: row.status }) },
  { title: 'Kind', key: 'kind', width: 120 },
  { title: 'Description', key: 'description' },
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

onMounted(() => void load())
</script>

<template>
  <div class="modules-page">
    <OjosPageHeader
      title="Module Registry"
      description="Installed and built-in modules with status, set membership, versions, and components."
      eyebrow="Admin"
    >
      <template #actions>
        <RouterLink to="/admin/modules/topology">
          <NButton secondary>Topology</NButton>
        </RouterLink>
        <NButton secondary :loading="refreshing" @click="load(true)">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="module-summary">
          <OjosStatCard label="Modules" :value="modules.length" tone="primary" />
          <OjosStatCard label="Sets" :value="sets.length" />
          <OjosStatCard label="Visible" :value="filteredModules.length" />
        </div>

        <OjosSection title="Registry">
          <NSpace class="module-filters">
            <NInput
              v-model:value="keyword"
              clearable
              placeholder="Search module id, name, or description"
              style="min-width: 280px"
            />
            <NSelect
              v-model:value="setFilter"
              :options="setOptions"
              clearable
              placeholder="Filter by set"
              style="width: 240px"
            />
            <NSelect
              v-model:value="statusFilter"
              :options="statusOptions"
              clearable
              placeholder="Filter by status"
              style="width: 180px"
            />
          </NSpace>

          <EmptyView v-if="filteredModules.length === 0" description="No matching modules" />
          <NDataTable
            v-else
            :columns="columns"
            :data="filteredModules"
            :pagination="{ pageSize: 12 }"
            :bordered="false"
          />
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.modules-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.module-summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.module-filters {
  width: 100%;
}

@media (max-width: 900px) {
  .module-summary {
    grid-template-columns: 1fr;
  }
}
</style>
