<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NTabPane,
  NTabs,
  type DataTableColumns,
} from 'naive-ui'

import { getModuleDetail } from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosModuleStatusTag from '../../components/oj/OjosModuleStatusTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type {
  ModuleComponentItem,
  ModuleDetailResponse,
  ModuleEdgeItem,
  ModuleFrontendRouteItem,
  ModuleGatewayRouteItem,
  ModuleInstallationItem,
  ModuleMenuItem,
  ModulePermissionItem,
} from '../../types/module'
import { formatDateTime } from '../../utils/format'

const route = useRoute()
const detail = ref<ModuleDetailResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)

const moduleId = computed(() => String(route.params.id || ''))

const edgeColumns: DataTableColumns<ModuleEdgeItem> = [
  { title: 'From', key: 'from_module_id', minWidth: 220 },
  { title: 'To', key: 'to_module_id', minWidth: 220 },
  { title: 'Type', key: 'edge_type', width: 130 },
  { title: 'Constraint', key: 'version_constraint', width: 160 },
  { title: 'Required', key: 'required', width: 100, render: (row) => (row.required ? 'yes' : 'no') },
]

const componentColumns: DataTableColumns<ModuleComponentItem> = [
  { title: 'Component', key: 'component_id', minWidth: 220 },
  { title: 'Type', key: 'component_type', width: 170 },
  {
    title: 'Status',
    key: 'status',
    width: 130,
    render: (row) => h(OjosModuleStatusTag, { status: row.status }),
  },
  { title: 'Config', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: 'Permission', key: 'permission_key', minWidth: 260 },
  { title: 'Description', key: 'description' },
]

const menuColumns: DataTableColumns<ModuleMenuItem> = [
  { title: 'Menu', key: 'menu_key', minWidth: 160 },
  { title: 'Title', key: 'title', minWidth: 160 },
  { title: 'Route', key: 'route_path', minWidth: 220 },
  { title: 'Permission', key: 'required_permission', minWidth: 220 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => (row.enabled ? 'yes' : 'no') },
]

const frontendRouteColumns: DataTableColumns<ModuleFrontendRouteItem> = [
  { title: 'Route', key: 'route_path', minWidth: 240 },
  { title: 'Name', key: 'route_name', minWidth: 180 },
  { title: 'Component', key: 'component_key', minWidth: 220 },
  { title: 'Permission', key: 'required_permission', minWidth: 220 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => (row.enabled ? 'yes' : 'no') },
]

const gatewayRouteColumns: DataTableColumns<ModuleGatewayRouteItem> = [
  { title: 'Prefix', key: 'prefix', minWidth: 220 },
  { title: 'Target service', key: 'target_service', minWidth: 180 },
  { title: 'Auth mode', key: 'auth_mode', width: 140 },
  { title: 'Enabled', key: 'enabled', width: 100, render: (row) => (row.enabled ? 'yes' : 'no') },
]

const installationColumns: DataTableColumns<ModuleInstallationItem> = [
  { title: 'Name', key: 'name', minWidth: 180 },
  { title: 'Version', key: 'version', width: 110 },
  {
    title: 'Status',
    key: 'status',
    width: 140,
    render: (row) => h(OjosModuleStatusTag, { status: row.status }),
  },
  { title: 'Enabled At', key: 'enabled_at', width: 180, render: (row) => formatDateTime(row.enabled_at) },
  { title: 'Disabled At', key: 'disabled_at', width: 180, render: (row) => formatDateTime(row.disabled_at) },
]

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    detail.value = await getModuleDetail(moduleId.value)
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
  <div class="module-detail-page">
    <OjosPageHeader
      :title="moduleId"
      :description="detail?.module.description || 'Module detail from the registry API.'"
      eyebrow="Module"
    >
      <template #actions>
        <RouterLink to="/admin/modules">
          <NButton secondary>Registry</NButton>
        </RouterLink>
        <RouterLink to="/admin/modules/topology">
          <NButton secondary>Topology</NButton>
        </RouterLink>
        <NButton secondary :loading="refreshing" @click="load(true)">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="detail">
        <div class="module-detail-summary">
          <OjosStatCard label="Components" :value="detail.components.length" tone="primary" />
          <OjosStatCard label="Dependencies" :value="detail.dependencies.length" />
          <OjosStatCard label="Dependents" :value="detail.dependents.length" />
          <OjosStatCard label="Permissions" :value="detail.permissions.length" />
        </div>

        <OjosSection title="Basic Information">
          <NDescriptions bordered :column="2">
            <NDescriptionsItem label="Module ID">{{ detail.module.module_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Name">{{ detail.module.name }}</NDescriptionsItem>
            <NDescriptionsItem label="Set">{{ detail.module.set_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Version">{{ detail.module.version }}</NDescriptionsItem>
            <NDescriptionsItem label="Status">
              <OjosModuleStatusTag :status="detail.module.status" />
            </NDescriptionsItem>
            <NDescriptionsItem label="Kind">{{ detail.module.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="Description" :span="2">
              {{ detail.module.description }}
            </NDescriptionsItem>
          </NDescriptions>
        </OjosSection>

        <OjosSection title="Registry Details">
          <NTabs type="line" animated>
            <NTabPane name="dependencies" tab="Dependencies">
              <EmptyView v-if="detail.dependencies.length === 0" description="No dependencies" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependencies" :bordered="false" />
            </NTabPane>
            <NTabPane name="dependents" tab="Dependents">
              <EmptyView v-if="detail.dependents.length === 0" description="No dependents" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependents" :bordered="false" />
            </NTabPane>
            <NTabPane name="components" tab="Components">
              <EmptyView v-if="detail.components.length === 0" description="No components" />
              <NDataTable
                v-else
                :columns="componentColumns"
                :data="detail.components"
                :pagination="{ pageSize: 8 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="permissions" tab="Permissions">
              <EmptyView v-if="detail.permissions.length === 0" description="No permissions" />
              <NDataTable
                v-else
                :columns="permissionColumns"
                :data="detail.permissions"
                :pagination="{ pageSize: 10 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="menus" tab="Menus">
              <EmptyView v-if="detail.menus.length === 0" description="No menus" />
              <NDataTable v-else :columns="menuColumns" :data="detail.menus" :bordered="false" />
            </NTabPane>
            <NTabPane name="frontend" tab="Frontend Routes">
              <EmptyView v-if="detail.frontend_routes.length === 0" description="No frontend routes" />
              <NDataTable
                v-else
                :columns="frontendRouteColumns"
                :data="detail.frontend_routes"
                :pagination="{ pageSize: 10 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="gateway" tab="Gateway Routes">
              <EmptyView v-if="detail.gateway_routes.length === 0" description="No gateway routes" />
              <NDataTable
                v-else
                :columns="gatewayRouteColumns"
                :data="detail.gateway_routes"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="health" tab="Health Checks">
              <EmptyView v-if="detail.health_checks.length === 0" description="No health checks" />
              <NDataTable
                v-else
                :columns="componentColumns"
                :data="detail.health_checks"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="installations" tab="Installations">
              <EmptyView v-if="detail.installations.length === 0" description="No installations" />
              <NDataTable
                v-else
                :columns="installationColumns"
                :data="detail.installations"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="manifest" tab="Manifest JSON">
              <OjosJsonViewer :value="detail.module.manifest || {}" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.module-detail-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.module-detail-summary {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

@media (max-width: 900px) {
  .module-detail-summary {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
