<script setup lang="ts">
import { h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NSpace, NTag, NTabPane, NTabs, type DataTableColumns } from 'naive-ui'

import { getOrchestratorSnapshot, listServiceSets } from '../../api/services'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import JsonViewer from '../../components/common/JsonViewer.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import type {
  ServiceComponentItem,
  ServiceDefinitionItem,
  ServiceEdgeItem,
  OrchestratorSnapshotResponse,
  ServiceSetItem,
  ServiceStatusComponent,
  ServiceTopologyEdge,
  ServiceTopologyNode,
  ServiceTopologyResponse,
} from '../../types/service'

const topology = ref<ServiceTopologyResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)

const setColumns: DataTableColumns<ServiceSetItem> = [
  { title: 'Set ID', key: 'set_id', width: 180 },
  { title: '名称', key: 'name', width: 180 },
  { title: '说明', key: 'description' },
  { title: '排序', key: 'sort_order', width: 100 },
]

const serviceDefinitionColumns: DataTableColumns<ServiceDefinitionItem> = [
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
  { title: 'Set', key: 'set_id', width: 160 },
  { title: '版本', key: 'version', width: 100 },
  { title: '状态', key: 'status', width: 120, render: (row) => statusTag(row.status) },
  { title: '类型', key: 'kind', width: 120 },
]

const dependencyEdgeColumns: DataTableColumns<ServiceEdgeItem> = [
  { title: '来源', key: 'from_service_id' },
  { title: '目标', key: 'to_service_id' },
  { title: '类型', key: 'edge_type', width: 120 },
  { title: '约束', key: 'version_constraint', width: 160 },
  { title: '必需', key: 'required', width: 100, render: (row) => (row.required ? '是' : '否') },
]

const topologyNodeColumns: DataTableColumns<ServiceTopologyNode> = [
  { title: '节点', key: 'id', minWidth: 280 },
  { title: '标签', key: 'label', minWidth: 180 },
  { title: '类型', key: 'type', width: 160 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '状态', key: 'status', width: 120, render: (row) => statusTag(row.status) },
  { title: '来源', key: 'source', width: 120 },
]

const topologyEdgeColumns: DataTableColumns<ServiceTopologyEdge> = [
  { title: '来源', key: 'from', minWidth: 280 },
  { title: '目标', key: 'to', minWidth: 280 },
  { title: '类型', key: 'type', width: 140 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '来源类型', key: 'source', width: 120 },
]

const componentColumns: DataTableColumns<ServiceComponentItem> = [
  { title: 'Service', key: 'service_id', width: 240 },
  { title: '组件', key: 'component_id', width: 220 },
  { title: '类型', key: 'component_type', width: 160 },
  { title: '状态', key: 'status', width: 120, render: (row) => statusTag(row.status) },
  { title: '配置', key: 'config', render: (row) => h(JsonViewer, { value: row.config }) },
]

function serviceStatusComponentToComponent(item: ServiceStatusComponent): ServiceComponentItem {
  return {
    service_id: item.service_id,
    component_id: item.component_id,
    component_type: item.type,
    status: item.status,
    config: item.config,
  }
}

function topologyFromSnapshot(
  sets: ServiceSetItem[],
  snapshot: OrchestratorSnapshotResponse,
): ServiceTopologyResponse {
  return {
    sets,
    nodes: snapshot.topology.nodes,
    edges: snapshot.topology.edges,
    components: snapshot.components.map(serviceStatusComponentToComponent),
    service_definitions: snapshot.topology.service_definitions,
    dependency_edges: snapshot.topology.dependency_edges,
  }
}

async function load(silent = false): Promise<void> {
  refreshing.value = silent
  loading.value = !silent
  error.value = null
  try {
    const [setResp, snapshot] = await Promise.all([listServiceSets(), getOrchestratorSnapshot()])
    topology.value = topologyFromSnapshot(setResp.sets, snapshot)
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function statusTag(status: string) {
  const type = status === 'ENABLED' || status === 'ok' ? 'success' : status.includes('FAILED') ? 'error' : 'default'
  return h(NTag, { type, size: 'small' }, { default: () => status })
}

onMounted(() => void load())
</script>

<template>
  <PageCard title="Service Topology">
    <template #headerExtra>
      <NSpace>
        <RouterLink to="/admin/services" class="header-link">Orchestrator Snapshot</RouterLink>
        <NButton size="small" secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </NSpace>
    </template>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NTabs v-else type="line" animated>
        <NTabPane name="sets" tab="Sets">
          <EmptyView v-if="!topology?.sets.length" description="暂无 Set" />
          <NDataTable v-else :columns="setColumns" :data="topology.sets" :pagination="{ pageSize: 10 }" :bordered="false" />
        </NTabPane>
        <NTabPane name="nodes" tab="Service 节点">
          <EmptyView v-if="!topology?.nodes.length" description="暂无 Service 节点" />
          <NDataTable v-else :columns="topologyNodeColumns" :data="topology.nodes" :pagination="{ pageSize: 10 }" :bordered="false" />
        </NTabPane>
        <NTabPane name="edges" tab="Service Link">
          <EmptyView v-if="!topology?.edges.length" description="暂无 Service Link" />
          <NDataTable v-else :columns="topologyEdgeColumns" :data="topology.edges" :pagination="{ pageSize: 10 }" :bordered="false" />
        </NTabPane>
        <NTabPane name="services" tab="Services">
          <EmptyView v-if="!topology?.service_definitions.length" description="暂无 Service 定义" />
          <NDataTable v-else :columns="serviceDefinitionColumns" :data="topology.service_definitions" :pagination="{ pageSize: 10 }" :bordered="false" />
        </NTabPane>
        <NTabPane name="dependencies" tab="依赖">
          <EmptyView v-if="!topology?.dependency_edges.length" description="暂无依赖" />
          <NDataTable v-else :columns="dependencyEdgeColumns" :data="topology.dependency_edges" :pagination="{ pageSize: 10 }" :bordered="false" />
        </NTabPane>
        <NTabPane name="components" tab="组件">
          <EmptyView v-if="!topology?.components.length" description="暂无组件" />
          <NDataTable v-else :columns="componentColumns" :data="topology.components" :pagination="{ pageSize: 8 }" :bordered="false" />
        </NTabPane>
      </NTabs>
    </template>
  </PageCard>
</template>
