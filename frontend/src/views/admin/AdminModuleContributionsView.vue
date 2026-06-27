<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
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

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: 'Permission', key: 'permission_key', minWidth: 240 },
  { title: 'Module', key: 'module_id', minWidth: 240 },
  { title: 'Description', key: 'description' },
]

const menuColumns: DataTableColumns<ModuleMenuItem> = [
  { title: 'Menu', key: 'menu_key', minWidth: 180 },
  { title: 'Title', key: 'title', minWidth: 180 },
  { title: 'Route', key: 'route_path', minWidth: 220 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Permission', key: 'required_permission', minWidth: 180 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const frontendRouteColumns: DataTableColumns<ModuleFrontendRouteItem> = [
  { title: 'Route', key: 'route_path', minWidth: 240 },
  { title: 'Name', key: 'route_name', minWidth: 180 },
  { title: 'Component', key: 'component_key', minWidth: 220 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Permission', key: 'required_permission', minWidth: 180 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const gatewayRouteColumns: DataTableColumns<ModuleGatewayRouteItem> = [
  { title: 'Prefix', key: 'prefix', minWidth: 220 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Target', key: 'target_service', minWidth: 160 },
  { title: 'Auth', key: 'auth_mode', width: 120 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
]

const runtimeRouteColumns: DataTableColumns<ModuleRuntimeRouteItem> = [
  { title: 'Prefix', key: 'prefix', minWidth: 220 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Target', key: 'target_service', minWidth: 160 },
  { title: 'Auth', key: 'auth_mode', width: 110 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
  {
    title: 'Conflicts',
    key: 'conflicts',
    minWidth: 260,
    render: (row) => row.conflicts.join('; ') || 'none',
  },
]

const componentColumns: DataTableColumns<ModuleRuntimeComponent> = [
  { title: 'Component', key: 'component_id', minWidth: 220 },
  { title: 'Type', key: 'type', width: 170 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Status', key: 'status', width: 120 },
  { title: 'Config', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const manifestItemColumns: DataTableColumns<ModuleRuntimeManifestItem> = [
  { title: 'ID', key: 'id', minWidth: 220 },
  { title: 'Type', key: 'type', width: 160 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Status', key: 'status', width: 120 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => enabledTag(row.enabled) },
  { title: 'Config', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const topologyNodeColumns: DataTableColumns<ModuleRuntimeTopologyNode> = [
  { title: 'Node', key: 'id', minWidth: 260 },
  { title: 'Label', key: 'label', minWidth: 180 },
  { title: 'Type', key: 'type', width: 150 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Source', key: 'source', width: 120 },
]

const topologyEdgeColumns: DataTableColumns<ModuleRuntimeTopologyEdge> = [
  { title: 'From', key: 'from', minWidth: 260 },
  { title: 'To', key: 'to', minWidth: 260 },
  { title: 'Type', key: 'type', width: 140 },
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Source', key: 'source', width: 120 },
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
  return h(NTag, { type: enabled ? 'success' : 'default', size: 'small' }, { default: () => (enabled ? 'yes' : 'no') })
}

onMounted(() => void load())
</script>

<template>
  <div class="module-contributions-page">
    <OjosPageHeader
      title="Module Contributions"
      description="Runtime snapshot viewer for registry-provided permissions, menus, routes, health checks, and topology."
      eyebrow="Kernel Runtime"
    >
      <template #actions>
        <NSpace>
          <NButton secondary :loading="refreshing" @click="reload()">Reload Runtime</NButton>
          <NButton secondary :loading="refreshing" @click="includeDisabled = !includeDisabled; load(true)">
            {{ includeDisabled ? 'Active Only' : 'Include Disabled' }}
          </NButton>
          <NButton secondary :loading="refreshing" @click="load(true)">Refresh</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="snapshot && routes">
        <div class="contribution-summary">
          <OjosStatCard label="Modules" :value="snapshot.modules.length" tone="primary" />
          <OjosStatCard label="Permissions" :value="snapshot.permissions.length" />
          <OjosStatCard label="Menus" :value="snapshot.menus.length" />
          <OjosStatCard label="Runtime Routes" :value="routes.routes.length" />
          <OjosStatCard label="Warnings" :value="warningCount" tone="warning" />
        </div>

        <OjosSection title="Runtime Contributions">
          <NTabs type="line" animated>
            <NTabPane name="permissions" tab="Permissions">
              <EmptyView v-if="snapshot.permissions.length === 0" description="No runtime permissions" />
              <NDataTable v-else :columns="permissionColumns" :data="snapshot.permissions" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="menus" tab="Menus">
              <EmptyView v-if="snapshot.menus.length === 0" description="No runtime menus" />
              <NDataTable v-else :columns="menuColumns" :data="snapshot.menus" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="frontend" tab="Frontend Routes">
              <EmptyView v-if="snapshot.frontend_routes.length === 0" description="No frontend routes" />
              <NDataTable v-else :columns="frontendRouteColumns" :data="snapshot.frontend_routes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="gateway" tab="Gateway Routes">
              <EmptyView v-if="snapshot.gateway_routes.length === 0" description="No gateway routes" />
              <NDataTable v-else :columns="gatewayRouteColumns" :data="snapshot.gateway_routes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="runtime-routes" tab="Route Table">
              <EmptyView v-if="routes.routes.length === 0" description="No runtime routes" />
              <NDataTable v-else :columns="runtimeRouteColumns" :data="routes.routes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="health" tab="Health">
              <EmptyView v-if="snapshot.health_checks.length === 0" description="No health checks" />
              <NDataTable v-else :columns="componentColumns" :data="snapshot.health_checks" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="storage" tab="Storage">
              <EmptyView v-if="snapshot.storage_buckets.length === 0" description="No storage buckets" />
              <NDataTable v-else :columns="manifestItemColumns" :data="snapshot.storage_buckets" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="operations" tab="Manifest Ops">
              <EmptyView v-if="snapshot.operations.length === 0" description="No manifest-level operations" />
              <NDataTable v-else :columns="manifestItemColumns" :data="snapshot.operations" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="topology-nodes" tab="Topology Nodes">
              <EmptyView v-if="snapshot.topology.nodes.length === 0" description="No topology nodes" />
              <NDataTable v-else :columns="topologyNodeColumns" :data="snapshot.topology.nodes" :pagination="{ pageSize: 12 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="topology-edges" tab="Topology Edges">
              <EmptyView v-if="snapshot.topology.edges.length === 0" description="No topology edges" />
              <NDataTable v-else :columns="topologyEdgeColumns" :data="snapshot.topology.edges" :pagination="{ pageSize: 12 }" :bordered="false" />
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
