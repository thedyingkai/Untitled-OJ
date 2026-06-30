<script setup lang="ts">
import { h, onMounted, ref } from 'vue'
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

import { getServiceDetail } from '../../api/services'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosServiceStatusTag from '../../components/oj/OjosServiceStatusTag.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import type {
  ServiceComponentItem,
  ServiceDetailResponse,
  ServiceEdgeItem,
  ServiceEndpointItem,
  ServiceFrontendRouteItem,
  ServiceGatewayRouteItem,
  ServiceMenuItem,
  ServicePermissionItem,
} from '../../types/service'

const route = useRoute()
const detail = ref<ServiceDetailResponse | null>(null)
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const serviceId = String(route.params.id || '')

const edgeColumns: DataTableColumns<ServiceEdgeItem> = [
  { title: '来源', key: 'from_service_id', minWidth: 220 },
  { title: '目标', key: 'to_service_id', minWidth: 220 },
  { title: '类型', key: 'edge_type', width: 130 },
  { title: '约束', key: 'version_constraint', width: 160 },
  { title: '必需', key: 'required', width: 100, render: (row) => (row.required ? '是' : '否') },
]

const componentColumns: DataTableColumns<ServiceComponentItem> = [
  { title: '组件', key: 'component_id', minWidth: 220 },
  { title: '类型', key: 'component_type', width: 170 },
  { title: '状态', key: 'status', width: 130, render: (row) => h(OjosServiceStatusTag, { status: row.status }) },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const permissionColumns: DataTableColumns<ServicePermissionItem> = [
  { title: '权限', key: 'permission_key', minWidth: 260 },
  { title: '说明', key: 'description' },
]

const menuColumns: DataTableColumns<ServiceMenuItem> = [
  { title: '菜单', key: 'menu_key', minWidth: 160 },
  { title: '标题', key: 'title', minWidth: 160 },
  { title: '路由', key: 'route_path', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 220 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const frontendRouteColumns: DataTableColumns<ServiceFrontendRouteItem> = [
  { title: '路由', key: 'route_path', minWidth: 240 },
  { title: '名称', key: 'route_name', minWidth: 180 },
  { title: '组件', key: 'component_key', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 220 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const gatewayRouteColumns: DataTableColumns<ServiceGatewayRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: '目标服务', key: 'target_service', minWidth: 180 },
  { title: '认证模式', key: 'auth_mode', width: 140 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const endpointColumns: DataTableColumns<ServiceEndpointItem> = [
  { title: 'Endpoint', key: 'endpoint', minWidth: 180 },
  { title: '协议', key: 'protocol', width: 110 },
  { title: '健康', key: 'health', width: 140, render: (row) => h(OjosServiceStatusTag, { status: row.health }) },
  { title: '可达', key: 'reachable', width: 100, render: (row) => (row.reachable ? '是' : '否') },
  { title: '健康路径', key: 'health_path', minWidth: 140 },
  { title: '备注', key: 'note', minWidth: 160 },
]

async function load(silent = false): Promise<void> {
  refreshing.value = silent
  loading.value = !silent
  error.value = null
  try {
    detail.value = await getServiceDetail(serviceId)
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
  <div class="service-detail-page">
    <OjosPageHeader
      :title="serviceId"
      :description="detail?.service.description || '来自 Orchestrator Snapshot API 的只读 Service 详情。'"
      eyebrow="Service"
    >
      <template #actions>
        <RouterLink to="/admin/services">
          <NButton secondary>Orchestrator Snapshot</NButton>
        </RouterLink>
        <RouterLink to="/admin/topology">
          <NButton secondary>Topology</NButton>
        </RouterLink>
        <NButton secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="detail">
        <div class="service-detail-summary">
          <OjosStatCard label="组件" :value="detail.components.length" tone="primary" />
          <OjosStatCard label="依赖" :value="detail.dependencies.length" />
          <OjosStatCard label="被依赖" :value="detail.dependents.length" />
          <OjosStatCard label="权限" :value="detail.permissions.length" />
        </div>

        <OjosSection title="基础信息">
          <NDescriptions bordered :column="2">
            <NDescriptionsItem label="Service ID">{{ detail.service.service_id }}</NDescriptionsItem>
            <NDescriptionsItem label="名称">{{ detail.service.name }}</NDescriptionsItem>
            <NDescriptionsItem label="版本">{{ detail.service.version }}</NDescriptionsItem>
            <NDescriptionsItem label="状态">
              <OjosServiceStatusTag :status="detail.service.status" />
            </NDescriptionsItem>
            <NDescriptionsItem label="类型">{{ detail.service.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="说明" :span="2">
              {{ detail.service.description }}
            </NDescriptionsItem>
          </NDescriptions>
        </OjosSection>

        <OjosSection title="只读快照详情">
          <NTabs type="line" animated>
            <NTabPane name="dependencies" tab="依赖">
              <EmptyView v-if="detail.dependencies.length === 0" description="暂无依赖" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependencies" :bordered="false" />
            </NTabPane>
            <NTabPane name="dependents" tab="被依赖">
              <EmptyView v-if="detail.dependents.length === 0" description="暂无被依赖 Service" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependents" :bordered="false" />
            </NTabPane>
            <NTabPane name="components" tab="组件">
              <EmptyView v-if="detail.components.length === 0" description="暂无组件" />
              <NDataTable v-else :columns="componentColumns" :data="detail.components" :pagination="{ pageSize: 8 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="permissions" tab="权限">
              <EmptyView v-if="detail.permissions.length === 0" description="暂无权限" />
              <NDataTable v-else :columns="permissionColumns" :data="detail.permissions" :pagination="{ pageSize: 10 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="menus" tab="菜单">
              <EmptyView v-if="detail.menus.length === 0" description="暂无菜单" />
              <NDataTable v-else :columns="menuColumns" :data="detail.menus" :bordered="false" />
            </NTabPane>
            <NTabPane name="frontend" tab="前端路由">
              <EmptyView v-if="detail.frontend_routes.length === 0" description="暂无前端路由" />
              <NDataTable v-else :columns="frontendRouteColumns" :data="detail.frontend_routes" :pagination="{ pageSize: 10 }" :bordered="false" />
            </NTabPane>
            <NTabPane name="gateway" tab="Gateway 路由">
              <EmptyView v-if="detail.gateway_routes.length === 0" description="暂无 Gateway 路由" />
              <NDataTable v-else :columns="gatewayRouteColumns" :data="detail.gateway_routes" :bordered="false" />
            </NTabPane>
            <NTabPane name="endpoints" tab="Endpoint">
              <EmptyView v-if="detail.endpoints.length === 0" description="暂无 Endpoint" />
              <NDataTable v-else :columns="endpointColumns" :data="detail.endpoints" :bordered="false" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.service-detail-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.service-detail-summary {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

@media (max-width: 900px) {
  .service-detail-summary {
    grid-template-columns: 1fr;
  }
}
</style>
