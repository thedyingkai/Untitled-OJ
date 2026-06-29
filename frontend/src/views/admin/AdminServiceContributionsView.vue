<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import { NButton, NDataTable, NTag, NTabPane, NTabs, type DataTableColumns } from 'naive-ui'

import { getOrchestratorRoutes, getOrchestratorSnapshot } from '../../api/services'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type {
  ServiceFrontendRouteItem,
  ServiceGatewayRouteItem,
  ServiceMenuItem,
  ServicePermissionItem,
  OrchestratorSnapshotItem,
  OrchestratorRouteItem,
  OrchestratorRoutesResponse,
  OrchestratorSnapshotResponse,
  ServiceStatusComponent,
  ServiceStatusItem,
  ServiceTopologyEdge,
  ServiceTopologyNode,
} from '../../types/service'

const snapshot = ref<OrchestratorSnapshotResponse | null>(null)
const routes = ref<OrchestratorRoutesResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const includeDisabled = ref(false)
const route = useRoute()
const selectedServiceId = computed(() => String(route.params.serviceId ?? ''))
const pageTitle = computed(() => (selectedServiceId.value ? 'Service UI 详情' : 'Service UI 总览'))

const visiblePermissions = computed(() => filterByService(snapshot.value?.permissions ?? []))
const visibleMenus = computed(() => filterByService(snapshot.value?.menus ?? []))
const visibleFrontendRoutes = computed(() => filterByService(snapshot.value?.frontend_routes ?? []))
const visibleGatewayRoutes = computed(() => filterByService(snapshot.value?.gateway_routes ?? []))
const visibleServiceRoutes = computed(() => filterByService(routes.value?.routes ?? []))
const visibleServices = computed(() => filterByService(snapshot.value?.services ?? []))
const visibleWorkers = computed(() => filterByService(snapshot.value?.workers ?? []))
const visibleHealthChecks = computed(() => filterByService(snapshot.value?.health_checks ?? []))
const visibleStorageBuckets = computed(() => filterByService(snapshot.value?.storage_buckets ?? []))
const visibleOperations = computed(() => filterByService(snapshot.value?.operations ?? []))
const visibleTopologyNodes = computed(() => filterByService(snapshot.value?.topology.nodes ?? []))
const visibleTopologyEdges = computed(() => filterByService(snapshot.value?.topology.edges ?? []))
const warningCount = computed(() => (snapshot.value?.warnings.length ?? 0) + (routes.value?.warnings.length ?? 0))

const permissionColumns: DataTableColumns<ServicePermissionItem> = [
  { title: '权限', key: 'permission_key', minWidth: 240 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '说明', key: 'description' },
]

const menuColumns: DataTableColumns<ServiceMenuItem> = [
  { title: '菜单', key: 'menu_key', minWidth: 180 },
  { title: '标题', key: 'title', minWidth: 180 },
  { title: '路由', key: 'route_path', minWidth: 220 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 180 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const frontendRouteColumns: DataTableColumns<ServiceFrontendRouteItem> = [
  { title: '路由', key: 'route_path', minWidth: 240 },
  { title: '名称', key: 'route_name', minWidth: 180 },
  { title: '组件', key: 'component_key', minWidth: 220 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 180 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const gatewayRouteColumns: DataTableColumns<ServiceGatewayRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '目标', key: 'target_service', minWidth: 160 },
  { title: '认证', key: 'auth_mode', width: 120 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const serviceRouteColumns: DataTableColumns<OrchestratorRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: 'Owner Service', key: 'owner_service_id', minWidth: 220 },
  { title: '目标 Service', key: 'service_id', minWidth: 160 },
  { title: '认证', key: 'auth_mode', width: 110 },
  { title: '状态', key: 'status', width: 120, render: (row) => routeStatusTag(row) },
  { title: '代理', key: 'proxy_enabled', width: 100, render: (row) => enabledTag(row.proxy_enabled) },
  {
    title: '阻塞 / 告警',
    key: 'blocked_by',
    minWidth: 260,
    render: (row) => [...row.blocked_by, ...row.conflicts, ...row.warnings].join('; ') || '无',
  },
]

const serviceStatusColumns: DataTableColumns<ServiceStatusItem> = [
  { title: 'Service', key: 'service_id', minWidth: 200 },
  { title: 'Owner', key: 'owner_service_id', minWidth: 220 },
  { title: '类型', key: 'kind', width: 120 },
  { title: 'Runtime', key: 'runtime', width: 120 },
  { title: '生命周期', key: 'lifecycle', width: 130 },
  { title: '状态', key: 'state', width: 130 },
  { title: '健康', key: 'health', width: 120 },
  { title: '路由', key: 'routes', minWidth: 220, render: (row) => row.routes.join(', ') || '无' },
  { title: '告警', key: 'warnings', minWidth: 260, render: (row) => [...row.blocked_by, ...row.warnings].join('; ') || '无' },
]

const componentColumns: DataTableColumns<ServiceStatusComponent> = [
  { title: '组件', key: 'component_id', minWidth: 220 },
  { title: '类型', key: 'type', width: 170 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '状态', key: 'status', width: 120 },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const manifestItemColumns: DataTableColumns<OrchestratorSnapshotItem> = [
  { title: 'ID', key: 'id', minWidth: 220 },
  { title: '类型', key: 'type', width: 160 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '状态', key: 'status', width: 120 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const topologyNodeColumns: DataTableColumns<ServiceTopologyNode> = [
  { title: '节点', key: 'id', minWidth: 260 },
  { title: '标签', key: 'label', minWidth: 180 },
  { title: '类型', key: 'type', width: 150 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '来源', key: 'source', width: 120 },
]

const topologyEdgeColumns: DataTableColumns<ServiceTopologyEdge> = [
  { title: '来源', key: 'from', minWidth: 260 },
  { title: '目标', key: 'to', minWidth: 260 },
  { title: '类型', key: 'type', width: 140 },
  { title: 'Service', key: 'service_id', minWidth: 220 },
  { title: '来源类型', key: 'source', width: 120 },
]

async function load(silent = false): Promise<void> {
  refreshing.value = silent
  loading.value = !silent
  error.value = null
  try {
    const [snapshotResp, routesResp] = await Promise.all([
      getOrchestratorSnapshot({ includeDisabled: includeDisabled.value }),
      getOrchestratorRoutes({ includeDisabled: includeDisabled.value }),
    ])
    snapshot.value = snapshotResp
    routes.value = routesResp
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

function enabledTag(enabled: boolean) {
  return h(NTag, { type: enabled ? 'success' : 'default', size: 'small' }, { default: () => (enabled ? '是' : '否') })
}

function routeStatusTag(row: OrchestratorRouteItem) {
  const type = row.status === 'active' ? 'success' : row.status === 'blocked' ? 'error' : 'default'
  return h(NTag, { type, size: 'small' }, { default: () => row.status || 'unknown' })
}

function filterByService<T extends { service_id: string }>(items: T[]): T[] {
  if (!selectedServiceId.value) return items
  return items.filter((item) => item.service_id === selectedServiceId.value)
}

function toggleIncludeDisabled(): void {
  includeDisabled.value = !includeDisabled.value
  void load(true)
}

onMounted(() => void load())
</script>

<template>
  <div class="service-contributions-page">
    <OjosPageHeader
      :title="pageTitle"
      :description="selectedServiceId ? `${selectedServiceId} 的只读 UI snapshot 视图。` : '来自 Orchestrator Snapshot 的权限、菜单、路由、健康检查和拓扑视图，Web Shell 不执行 apply。'"
      eyebrow="只读快照"
    >
      <template #actions>
        <NButton secondary :loading="refreshing" @click="toggleIncludeDisabled">
          {{ includeDisabled ? '仅看启用' : '包含禁用' }}
        </NButton>
        <NButton secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="snapshot && routes">
        <div class="contribution-summary">
          <OjosStatCard label="Services" :value="snapshot.service_definitions.length" tone="primary" />
          <OjosStatCard label="权限" :value="visiblePermissions.length" />
          <OjosStatCard label="菜单" :value="visibleMenus.length" />
          <OjosStatCard label="运行服务" :value="visibleServices.length + visibleWorkers.length" />
          <OjosStatCard label="Service 路由" :value="visibleServiceRoutes.length" />
          <OjosStatCard label="告警" :value="warningCount" tone="warning" />
        </div>

        <OjosSection title="Orchestrator Snapshot">
          <NTabs type="line" animated>
            <NTabPane name="permissions" tab="权限">
              <EmptyView v-if="visiblePermissions.length === 0" description="暂无 Service 权限" />
              <NDataTable v-else :columns="permissionColumns" :data="visiblePermissions" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="menus" tab="菜单">
              <EmptyView v-if="visibleMenus.length === 0" description="暂无 Service 菜单" />
              <NDataTable v-else :columns="menuColumns" :data="visibleMenus" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="frontend" tab="前端路由">
              <EmptyView v-if="visibleFrontendRoutes.length === 0" description="暂无前端路由" />
              <NDataTable v-else :columns="frontendRouteColumns" :data="visibleFrontendRoutes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="gateway" tab="Gateway 路由">
              <EmptyView v-if="visibleGatewayRoutes.length === 0" description="暂无 Gateway 路由" />
              <NDataTable v-else :columns="gatewayRouteColumns" :data="visibleGatewayRoutes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="service-routes" tab="Service 路由">
              <EmptyView v-if="visibleServiceRoutes.length === 0" description="暂无 Service 路由" />
              <NDataTable v-else :columns="serviceRouteColumns" :data="visibleServiceRoutes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="services" tab="服务状态">
              <EmptyView v-if="visibleServices.length + visibleWorkers.length === 0" description="暂无服务状态" />
              <NDataTable v-else :columns="serviceStatusColumns" :data="[...visibleServices, ...visibleWorkers]" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="health" tab="健康检查">
              <EmptyView v-if="visibleHealthChecks.length === 0" description="暂无健康检查" />
              <NDataTable v-else :columns="componentColumns" :data="visibleHealthChecks" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="storage" tab="存储">
              <EmptyView v-if="visibleStorageBuckets.length === 0" description="暂无存储声明" />
              <NDataTable v-else :columns="manifestItemColumns" :data="visibleStorageBuckets" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="operations" tab="操作记录">
              <EmptyView v-if="visibleOperations.length === 0" description="暂无操作记录声明" />
              <NDataTable v-else :columns="manifestItemColumns" :data="visibleOperations" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="topology" tab="Topology">
              <EmptyView v-if="visibleTopologyNodes.length === 0" description="暂无拓扑节点" />
              <NDataTable v-else :columns="topologyNodeColumns" :data="visibleTopologyNodes" :pagination="{ pageSize: 12 }" :bordered="false" />
              <NDataTable v-if="visibleTopologyEdges.length > 0" class="topology-edge-table" :columns="topologyEdgeColumns" :data="visibleTopologyEdges" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.service-contributions-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.contribution-summary {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 12px;
}

.topology-edge-table {
  margin-top: 16px;
}

@media (max-width: 1100px) {
  .contribution-summary {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .contribution-summary {
    grid-template-columns: 1fr;
  }
}
</style>
