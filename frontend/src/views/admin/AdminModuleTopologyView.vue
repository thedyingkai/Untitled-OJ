<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NSpace, NTag, NTabPane, NTabs, type DataTableColumns } from 'naive-ui'

import { getModuleTopology } from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import JsonViewer from '../../components/common/JsonViewer.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import type {
  ModuleComponentItem,
  ModuleEdgeItem,
  ModuleNodeItem,
  ModuleSetItem,
  ModuleTopologyResponse,
} from '../../types/module'

const topology = ref<ModuleTopologyResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)

const setColumns: DataTableColumns<ModuleSetItem> = [
  { title: 'Set ID', key: 'set_id', width: 180 },
  { title: 'Name', key: 'name', width: 180 },
  { title: 'Description', key: 'description' },
  { title: 'Order', key: 'sort_order', width: 100 },
]

const nodeColumns = computed<DataTableColumns<ModuleNodeItem>>(() => [
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
  { title: 'Set', key: 'set_id', width: 160 },
  { title: 'Version', key: 'version', width: 100 },
  { title: 'Status', key: 'status', width: 120, render: (row) => hStatus(row.status) },
  { title: 'Kind', key: 'kind', width: 120 },
])

const edgeColumns: DataTableColumns<ModuleEdgeItem> = [
  { title: 'From', key: 'from_module_id' },
  { title: 'To', key: 'to_module_id' },
  { title: 'Type', key: 'edge_type', width: 120 },
  { title: 'Constraint', key: 'version_constraint', width: 160 },
  { title: 'Required', key: 'required', width: 100, render: (row) => (row.required ? 'yes' : 'no') },
]

const componentColumns: DataTableColumns<ModuleComponentItem> = [
  { title: 'Module', key: 'module_id', width: 240 },
  { title: 'Component', key: 'component_id', width: 220 },
  { title: 'Type', key: 'component_type', width: 160 },
  { title: 'Status', key: 'status', width: 120, render: (row) => hStatus(row.status) },
  { title: 'Config', key: 'config', render: (row) => h(JsonViewer, { value: row.config }) },
]

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    topology.value = await getModuleTopology()
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function hStatus(status: string) {
  const type = status === 'ENABLED' ? 'success' : status.includes('FAILED') ? 'error' : 'default'
  return h(NTag, { type, size: 'small' }, { default: () => status })
}

onMounted(() => void load())
</script>

<template>
  <PageCard title="Module Topology">
    <template #headerExtra>
      <NSpace>
        <RouterLink to="/admin/modules" class="header-link">Registry</RouterLink>
        <NButton size="small" secondary :loading="refreshing" @click="load(true)">Refresh</NButton>
      </NSpace>
    </template>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NTabs v-else type="line" animated>
        <NTabPane name="sets" tab="Sets">
          <EmptyView v-if="!topology?.sets.length" description="No module sets" />
          <NDataTable
            v-else
            :columns="setColumns"
            :data="topology.sets"
            :pagination="{ pageSize: 10 }"
            :bordered="false"
          />
        </NTabPane>
        <NTabPane name="nodes" tab="Nodes">
          <EmptyView v-if="!topology?.nodes.length" description="No module nodes" />
          <NDataTable
            v-else
            :columns="nodeColumns"
            :data="topology.nodes"
            :pagination="{ pageSize: 10 }"
            :bordered="false"
          />
        </NTabPane>
        <NTabPane name="edges" tab="Edges">
          <EmptyView v-if="!topology?.edges.length" description="No dependency edges" />
          <NDataTable
            v-else
            :columns="edgeColumns"
            :data="topology.edges"
            :pagination="{ pageSize: 10 }"
            :bordered="false"
          />
        </NTabPane>
        <NTabPane name="components" tab="Components">
          <EmptyView v-if="!topology?.components.length" description="No components" />
          <NDataTable
            v-else
            :columns="componentColumns"
            :data="topology.components"
            :pagination="{ pageSize: 8 }"
            :bordered="false"
          />
        </NTabPane>
      </NTabs>
    </template>
  </PageCard>
</template>
