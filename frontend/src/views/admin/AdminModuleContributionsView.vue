<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import {
  NButton,
  NDataTable,
  NSpace,
  NTabPane,
  NTabs,
  NTag,
  type DataTableColumns,
} from 'naive-ui'

import {
  getModuleRuntimeRoutes,
  getModuleRuntimeSnapshot,
  reloadModuleRuntime,
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
  ModuleFrontendRouteItem,
  ModuleGatewayRouteItem,
  ModuleMenuItem,
  ModulePermissionItem,
  ModuleRuntimeComponent,
  ModuleRuntimeManifestItem,
  ModuleRuntimeRouteItem,
  ModuleRuntimeRoutesResponse,
  ModuleRuntimeService,
  ModuleRuntimeSnapshotResponse,
  ModuleRuntimeTopologyEdge,
  ModuleRuntimeTopologyNode,
} from '../../types/module'

const snapshot = ref<ModuleRuntimeSnapshotResponse | null>(null)
const routes = ref<ModuleRuntimeRoutesResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const includeDisabled = ref(false)
const route = useRoute()
const selectedModuleId = computed(() => String(route.params.moduleId ?? ''))
const pageTitle = computed(() => selectedModuleId.value ? '模块贡献' : '模块贡献总览')

const visiblePermissions = computed(() => filterByModule(snapshot.value?.permissions ?? []))
const visibleMenus = computed(() => filterByModule(snapshot.value?.menus ?? []))
const visibleFrontendRoutes = computed(() => filterByModule(snapshot.value?.frontend_routes ?? []))
const visibleGatewayRoutes = computed(() => filterByModule(snapshot.value?.gateway_routes ?? []))
const visibleRuntimeRoutes = computed(() => filterByModule(routes.value?.routes ?? []))
const visibleServices = computed(() => filterByModule(snapshot.value?.services ?? []))
const visibleWorkers = computed(() => filterByModule(snapshot.value?.workers ?? []))
const visibleHealthChecks = computed(() => filterByModule(snapshot.value?.health_checks ?? []))
const visibleStorageBuckets = computed(() => filterByModule(snapshot.value?.storage_buckets ?? []))
const visibleOperations = computed(() => filterByModule(snapshot.value?.operations ?? []))
const visibleTopologyNodes = computed(() => filterByModule(snapshot.value?.topology.nodes ?? []))
const visibleTopologyEdges = computed(() => filterByModule(snapshot.value?.topology.edges ?? []))

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: '权限', key: 'permission_key', minWidth: 240 },
  { title: '模块', key: 'module_id', minWidth: 240 },
  { title: '说明', key: 'description' },
]

const menuColumns: DataTableColumns<ModuleMenuItem> = [
  { title: '菜单', key: 'menu_key', minWidth: 180 },
  { title: '标题', key: 'title', minWidth: 180 },
  { title: '路由', key: 'route_path', minWidth: 220 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 180 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const frontendRouteColumns: DataTableColumns<ModuleFrontendRouteItem> = [
  { title: '路由', key: 'route_path', minWidth: 240 },
  { title: '名称', key: 'route_name', minWidth: 180 },
  { title: '组件', key: 'component_key', minWidth: 220 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 180 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const gatewayRouteColumns: DataTableColumns<ModuleGatewayRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '目标', key: 'target_service', minWidth: 160 },
  { title: '认证', key: 'auth_mode', width: 120 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const runtimeRouteColumns: DataTableColumns<ModuleRuntimeRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '服务', key: 'service_id', minWidth: 160 },
  { title: '认证', key: 'auth_mode', width: 110 },
  { title: '状态', key: 'status', width: 120, render: (row) => routeStatusTag(row) },
  { title: '代理', key: 'proxy_enabled', width: 100, render: (row) => enabledTag(row.proxy_enabled) },
  {
    title: '阻塞 / 警告',
    key: 'blocked_by',
    minWidth: 260,
    render: (row) => [...row.blocked_by, ...row.conflicts, ...row.warnings].join('; ') || '无',
  },
]

const runtimeServiceColumns: DataTableColumns<ModuleRuntimeService> = [
  { title: '服务', key: 'service_id', minWidth: 200 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '类型', key: 'kind', width: 120 },
  { title: 'Runtime', key: 'runtime', width: 120 },
  { title: '生命周期', key: 'lifecycle', width: 130 },
  { title: '状态', key: 'state', width: 130 },
  { title: '健康', key: 'health', width: 120 },
  { title: '路由', key: 'routes', minWidth: 220, render: (row) => row.routes.join(', ') || '无' },
  {
    title: '警告',
    key: 'warnings',
    minWidth: 260,
    render: (row) => [...row.blocked_by, ...row.warnings].join('; ') || '无',
  },
]

const componentColumns: DataTableColumns<ModuleRuntimeComponent> = [
  { title: '组件', key: 'component_id', minWidth: 220 },
  { title: '类型', key: 'type', width: 170 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '状态', key: 'status', width: 120 },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const manifestItemColumns: DataTableColumns<ModuleRuntimeManifestItem> = [
  { title: 'ID', key: 'id', minWidth: 220 },
  { title: '类型', key: 'type', width: 160 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '状态', key: 'status', width: 120 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const topologyNodeColumns: DataTableColumns<ModuleRuntimeTopologyNode> = [
  { title: '节点', key: 'id', minWidth: 260 },
  { title: '标签', key: 'label', minWidth: 180 },
  { title: '类型', key: 'type', width: 150 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '来源', key: 'source', width: 120 },
]

const topologyEdgeColumns: DataTableColumns<ModuleRuntimeTopologyEdge> = [
  { title: '来源', key: 'from', minWidth: 260 },
  { title: '目标', key: 'to', minWidth: 260 },
  { title: '类型', key: 'type', width: 140 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '来源类型', key: 'source', width: 120 },
]

const warningCount = computed(() => (snapshot.value?.warnings.length ?? 0) + (routes.value?.warnings.length ?? 0))

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    const [snapshotResp, routesResp] = await Promise.all([
      getModuleRuntimeSnapshot({ includeDisabled: includeDisabled.value }),
      getModuleRuntimeRoutes({ includeDisabled: includeDisabled.value }),
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

async function reload(): Promise<void> {
  refreshing.value = true
  error.value = null
  try {
    routes.value = await reloadModuleRuntime({ includeDisabled: includeDisabled.value })
    snapshot.value = await getModuleRuntimeSnapshot({ includeDisabled: includeDisabled.value })
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    refreshing.value = false
  }
}

function enabledTag(enabled: boolean) {
  return h(NTag, { type: enabled ? 'success' : 'default', size: 'small' }, { default: () => (enabled ? '是' : '否') })
}

function routeStatusTag(row: ModuleRuntimeRouteItem) {
  const type = row.status === 'active' ? 'success' : row.status === 'blocked' ? 'error' : 'default'
  return h(NTag, { type, size: 'small' }, { default: () => row.status || 'unknown' })
}

function filterByModule<T extends { module_id: string }>(items: T[]): T[] {
  if (!selectedModuleId.value) return items
  return items.filter((item) => item.module_id === selectedModuleId.value)
}

onMounted(() => void load())
</script>

<template>
  <div class="module-contributions-page">
    <OjosPageHeader
      :title="pageTitle"
      :description="selectedModuleId ? `${selectedModuleId} 的安全注册表视图；未知 component_key 仅按 metadata 展示。` : '来自 Runtime Snapshot 的权限、菜单、路由、健康检查和拓扑视图。Web Shell 不执行危险 apply。'"
      eyebrow="Kernel Runtime"
    >
      <template #actions>
        <NSpace>
          <NButton secondary :loading="refreshing" @click="reload()">重载 Runtime</NButton>
          <NButton secondary :loading="refreshing" @click="includeDisabled = !includeDisabled; load(true)">
            {{ includeDisabled ? '仅看启用' : '包含禁用' }}
          </NButton>
          <NButton secondary :loading="refreshing" @click="load(true)">刷新</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="snapshot && routes">
        <div class="contribution-summary">
          <OjosStatCard label="模块" :value="snapshot.modules.length" tone="primary" />
          <OjosStatCard label="权限" :value="visiblePermissions.length" />
          <OjosStatCard label="Menus" :value="visibleMenus.length" />
          <OjosStatCard label="服务" :value="visibleServices.length + visibleWorkers.length" />
          <OjosStatCard label="Runtime 路由" :value="visibleRuntimeRoutes.length" />
          <OjosStatCard label="警告" :value="warningCount" tone="warning" />
        </div>

        <OjosSection title="Runtime 贡献">
          <NTabs type="line" animated>
            <NTabPane name="permissions" tab="权限">
              <EmptyView v-if="visiblePermissions.length === 0" description="暂无 Runtime 权限" />
              <NDataTable v-else :columns="permissionColumns" :data="visiblePermissions" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="menus" tab="菜单">
              <EmptyView v-if="visibleMenus.length === 0" description="暂无 Runtime 菜单" />
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
            <NTabPane name="runtime-routes" tab="路由表">
              <EmptyView v-if="visibleRuntimeRoutes.length === 0" description="暂无 Runtime 路由" />
              <NDataTable v-else :columns="runtimeRouteColumns" :data="visibleRuntimeRoutes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="services" tab="服务">
              <EmptyView v-if="visibleServices.length === 0" description="暂无 Runtime 服务" />
              <NDataTable v-else :columns="runtimeServiceColumns" :data="visibleServices" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="workers" tab="Worker">
              <EmptyView v-if="visibleWorkers.length === 0" description="暂无 Runtime Worker" />
              <NDataTable v-else :columns="runtimeServiceColumns" :data="visibleWorkers" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="health" tab="健康">
              <EmptyView v-if="visibleHealthChecks.length === 0" description="暂无健康检查" />
              <NDataTable v-else :columns="componentColumns" :data="visibleHealthChecks" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="storage" tab="存储">
              <EmptyView v-if="visibleStorageBuckets.length === 0" description="暂无存储桶" />
              <NDataTable v-else :columns="manifestItemColumns" :data="visibleStorageBuckets" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="operations" tab="Manifest 操作">
              <EmptyView v-if="visibleOperations.length === 0" description="暂无 manifest 级操作" />
              <NDataTable v-else :columns="manifestItemColumns" :data="visibleOperations" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="topology-nodes" tab="拓扑节点">
              <EmptyView v-if="visibleTopologyNodes.length === 0" description="暂无拓扑节点" />
              <NDataTable v-else :columns="topologyNodeColumns" :data="visibleTopologyNodes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="topology-edges" tab="拓扑边">
              <EmptyView v-if="visibleTopologyEdges.length === 0" description="暂无拓扑边" />
              <NDataTable v-else :columns="topologyEdgeColumns" :data="visibleTopologyEdges" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.module-contributions-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.contribution-summary {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 12px;
}

@media (max-width: 1100px) {
  .contribution-summary {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 680px) {
  .contribution-summary {
    grid-template-columns: 1fr;
  }
}
</style>
