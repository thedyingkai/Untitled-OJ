<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NSpace, NTag, NTabs, NTabPane, type DataTableColumns } from 'naive-ui'

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
  { title: 'set_id', key: 'set_id' },
  { title: 'name', key: 'name' },
  { title: 'description', key: 'description' },
  { title: 'sort_order', key: 'sort_order', width: 120 },
]

const nodeColumns = computed<DataTableColumns<ModuleNodeItem>>(() => [
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
  { title: 'set_id', key: 'set_id' },
  { title: 'version', key: 'version', width: 100 },
  { title: 'status', key: 'status', width: 120, render: (row) => hStatus(row.status) },
  { title: 'kind', key: 'kind', width: 120 },
])

const edgeColumns: DataTableColumns<ModuleEdgeItem> = [
  { title: 'from_module_id', key: 'from_module_id' },
  { title: 'to_module_id', key: 'to_module_id' },
  { title: 'edge_type', key: 'edge_type', width: 120 },
  { title: 'version_constraint', key: 'version_constraint', width: 160 },
  { title: 'required', key: 'required', width: 100, render: (row) => (row.required ? 'yes' : 'no') },
]

const componentColumns: DataTableColumns<ModuleComponentItem> = [
  { title: 'module_id', key: 'module_id' },
  { title: 'component_id', key: 'component_id' },
  { title: 'component_type', key: 'component_type' },
  { title: 'status', key: 'status', width: 120, render: (row) => hStatus(row.status) },
  { title: 'config', key: 'config', render: (row) => h(JsonViewer, { value: row.config }) },
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
  return h(NTag, { type, size: 'small', round: true }, { default: () => status })
}

onMounted(() => void load())
</script>

<template>
  <PageCard title="模块拓扑">
    <template #headerExtra>
      <NSpace>
        <RouterLink to="/admin/modules">模块列表</RouterLink>
        <NButton size="small" secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </NSpace>
    </template>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NTabs v-else type="line">
        <NTabPane name="sets" tab="集合">
          <EmptyView v-if="!topology?.sets.length" description="没有模块集合" />
          <NDataTable v-else :columns="setColumns" :data="topology.sets" :pagination="{ pageSize: 10 }" />
        </NTabPane>
        <NTabPane name="nodes" tab="模块节点">
          <EmptyView v-if="!topology?.nodes.length" description="没有模块节点" />
          <NDataTable v-else :columns="nodeColumns" :data="topology.nodes" :pagination="{ pageSize: 10 }" />
        </NTabPane>
        <NTabPane name="edges" tab="依赖边">
          <EmptyView v-if="!topology?.edges.length" description="没有依赖边" />
          <NDataTable v-else :columns="edgeColumns" :data="topology.edges" :pagination="{ pageSize: 10 }" />
        </NTabPane>
        <NTabPane name="components" tab="组件">
          <EmptyView v-if="!topology?.components.length" description="没有组件" />
          <NDataTable
            v-else
            :columns="componentColumns"
            :data="topology.components"
            :pagination="{ pageSize: 8 }"
          />
        </NTabPane>
      </NTabs>
    </template>
  </PageCard>
</template>
