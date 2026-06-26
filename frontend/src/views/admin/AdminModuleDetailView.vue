<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NSpace,
  NTag,
  NTabs,
  NTabPane,
  type DataTableColumns,
} from 'naive-ui'

import { getModuleDetail } from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import JsonViewer from '../../components/common/JsonViewer.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
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

const route = useRoute()
const detail = ref<ModuleDetailResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)

const moduleId = computed(() => String(route.params.id || ''))

const edgeColumns: DataTableColumns<ModuleEdgeItem> = [
  { title: 'from_module_id', key: 'from_module_id' },
  { title: 'to_module_id', key: 'to_module_id' },
  { title: 'edge_type', key: 'edge_type' },
  { title: 'version_constraint', key: 'version_constraint' },
  { title: 'required', key: 'required', render: (row) => (row.required ? 'yes' : 'no') },
]

const componentColumns: DataTableColumns<ModuleComponentItem> = [
  { title: 'component_id', key: 'component_id' },
  { title: 'component_type', key: 'component_type' },
  { title: 'status', key: 'status', render: (row) => hStatus(row.status) },
  { title: 'config', key: 'config', render: (row) => h(JsonViewer, { value: row.config }) },
]

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: 'permission_key', key: 'permission_key' },
  { title: 'description', key: 'description' },
]

const menuColumns: DataTableColumns<ModuleMenuItem> = [
  { title: 'menu_key', key: 'menu_key' },
  { title: 'title', key: 'title' },
  { title: 'route_path', key: 'route_path' },
  { title: 'required_permission', key: 'required_permission' },
  { title: 'enabled', key: 'enabled', render: (row) => (row.enabled ? 'yes' : 'no') },
]

const frontendRouteColumns: DataTableColumns<ModuleFrontendRouteItem> = [
  { title: 'route_path', key: 'route_path' },
  { title: 'route_name', key: 'route_name' },
  { title: 'component_key', key: 'component_key' },
  { title: 'required_permission', key: 'required_permission' },
  { title: 'enabled', key: 'enabled', render: (row) => (row.enabled ? 'yes' : 'no') },
]

const gatewayRouteColumns: DataTableColumns<ModuleGatewayRouteItem> = [
  { title: 'prefix', key: 'prefix' },
  { title: 'target_service', key: 'target_service' },
  { title: 'auth_mode', key: 'auth_mode' },
  { title: 'enabled', key: 'enabled', render: (row) => (row.enabled ? 'yes' : 'no') },
]

const installationColumns: DataTableColumns<ModuleInstallationItem> = [
  { title: 'name', key: 'name' },
  { title: 'version', key: 'version' },
  { title: 'status', key: 'status', render: (row) => hStatus(row.status) },
  { title: 'enabled_at', key: 'enabled_at' },
  { title: 'disabled_at', key: 'disabled_at' },
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

function hStatus(status: string) {
  const type = status === 'ENABLED' ? 'success' : status.includes('FAILED') ? 'error' : 'default'
  return h(NTag, { type, size: 'small', round: true }, { default: () => status })
}

onMounted(() => void load())
</script>

<template>
  <PageCard :title="moduleId">
    <template #headerExtra>
      <NSpace>
        <RouterLink to="/admin/modules">模块列表</RouterLink>
        <RouterLink to="/admin/modules/topology">拓扑视图</RouterLink>
        <NButton size="small" secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </NSpace>
    </template>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NSpace v-else-if="detail" vertical size="large">
        <NDescriptions bordered :column="2">
          <NDescriptionsItem label="module_id">{{ detail.module.module_id }}</NDescriptionsItem>
          <NDescriptionsItem label="name">{{ detail.module.name }}</NDescriptionsItem>
          <NDescriptionsItem label="set_id">{{ detail.module.set_id }}</NDescriptionsItem>
          <NDescriptionsItem label="version">{{ detail.module.version }}</NDescriptionsItem>
          <NDescriptionsItem label="status">
            <NTag size="small" round>{{ detail.module.status }}</NTag>
          </NDescriptionsItem>
          <NDescriptionsItem label="kind">{{ detail.module.kind }}</NDescriptionsItem>
          <NDescriptionsItem label="description" :span="2">
            {{ detail.module.description }}
          </NDescriptionsItem>
        </NDescriptions>

        <NTabs type="line">
          <NTabPane name="dependencies" tab="依赖模块">
            <EmptyView v-if="detail.dependencies.length === 0" description="没有依赖模块" />
            <NDataTable v-else :columns="edgeColumns" :data="detail.dependencies" />
          </NTabPane>
          <NTabPane name="dependents" tab="被依赖模块">
            <EmptyView v-if="detail.dependents.length === 0" description="没有被依赖模块" />
            <NDataTable v-else :columns="edgeColumns" :data="detail.dependents" />
          </NTabPane>
          <NTabPane name="components" tab="组件">
            <EmptyView v-if="detail.components.length === 0" description="没有组件" />
            <NDataTable
              v-else
              :columns="componentColumns"
              :data="detail.components"
              :pagination="{ pageSize: 8 }"
            />
          </NTabPane>
          <NTabPane name="permissions" tab="权限点">
            <EmptyView v-if="detail.permissions.length === 0" description="没有权限点" />
            <NDataTable
              v-else
              :columns="permissionColumns"
              :data="detail.permissions"
              :pagination="{ pageSize: 10 }"
            />
          </NTabPane>
          <NTabPane name="menus" tab="菜单">
            <EmptyView v-if="detail.menus.length === 0" description="没有菜单" />
            <NDataTable v-else :columns="menuColumns" :data="detail.menus" />
          </NTabPane>
          <NTabPane name="frontend" tab="前端路由">
            <EmptyView v-if="detail.frontend_routes.length === 0" description="没有前端路由" />
            <NDataTable
              v-else
              :columns="frontendRouteColumns"
              :data="detail.frontend_routes"
              :pagination="{ pageSize: 10 }"
            />
          </NTabPane>
          <NTabPane name="gateway" tab="Gateway 路由">
            <EmptyView v-if="detail.gateway_routes.length === 0" description="没有 Gateway 路由" />
            <NDataTable v-else :columns="gatewayRouteColumns" :data="detail.gateway_routes" />
          </NTabPane>
          <NTabPane name="health" tab="健康检查">
            <EmptyView v-if="detail.health_checks.length === 0" description="没有健康检查组件" />
            <NDataTable v-else :columns="componentColumns" :data="detail.health_checks" />
          </NTabPane>
          <NTabPane name="installations" tab="安装状态">
            <EmptyView v-if="detail.installations.length === 0" description="没有安装状态" />
            <NDataTable v-else :columns="installationColumns" :data="detail.installations" />
          </NTabPane>
          <NTabPane name="manifest" tab="manifest JSON">
            <JsonViewer :value="detail.module.manifest || {}" />
          </NTabPane>
        </NTabs>
      </NSpace>
    </template>
  </PageCard>
</template>
