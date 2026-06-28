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
  { title: '来源', key: 'from_module_id', minWidth: 220 },
  { title: '目标', key: 'to_module_id', minWidth: 220 },
  { title: '类型', key: 'edge_type', width: 130 },
  { title: '约束', key: 'version_constraint', width: 160 },
  { title: '必需', key: 'required', width: 100, render: (row) => (row.required ? '是' : '否') },
]

const componentColumns: DataTableColumns<ModuleComponentItem> = [
  { title: '组件', key: 'component_id', minWidth: 220 },
  { title: '类型', key: 'component_type', width: 170 },
  {
    title: '状态',
    key: 'status',
    width: 130,
    render: (row) => h(OjosModuleStatusTag, { status: row.status }),
  },
  { title: '配置', key: 'config', render: (row) => h(OjosJsonViewer, { value: row.config }) },
]

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: '权限', key: 'permission_key', minWidth: 260 },
  { title: '说明', key: 'description' },
]

const menuColumns: DataTableColumns<ModuleMenuItem> = [
  { title: '菜单', key: 'menu_key', minWidth: 160 },
  { title: '标题', key: 'title', minWidth: 160 },
  { title: '路由', key: 'route_path', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 220 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const frontendRouteColumns: DataTableColumns<ModuleFrontendRouteItem> = [
  { title: '路由', key: 'route_path', minWidth: 240 },
  { title: '名称', key: 'route_name', minWidth: 180 },
  { title: '组件', key: 'component_key', minWidth: 220 },
  { title: '权限', key: 'required_permission', minWidth: 220 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const gatewayRouteColumns: DataTableColumns<ModuleGatewayRouteItem> = [
  { title: '前缀', key: 'prefix', minWidth: 220 },
  { title: '目标服务', key: 'target_service', minWidth: 180 },
  { title: '认证模式', key: 'auth_mode', width: 140 },
  { title: '启用', key: 'enabled', width: 100, render: (row) => (row.enabled ? '是' : '否') },
]

const installationColumns: DataTableColumns<ModuleInstallationItem> = [
  { title: '名称', key: 'name', minWidth: 180 },
  { title: '版本', key: 'version', width: 110 },
  {
    title: '状态',
    key: 'status',
    width: 140,
    render: (row) => h(OjosModuleStatusTag, { status: row.status }),
  },
  { title: '启用时间', key: 'enabled_at', width: 180, render: (row) => formatDateTime(row.enabled_at) },
  { title: '禁用时间', key: 'disabled_at', width: 180, render: (row) => formatDateTime(row.disabled_at) },
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
      :description="detail?.module.description || '来自模块注册表 API 的模块详情。'"
      eyebrow="模块"
    >
      <template #actions>
        <RouterLink to="/admin/modules">
          <NButton secondary>注册表</NButton>
        </RouterLink>
        <RouterLink to="/admin/modules/topology">
          <NButton secondary>拓扑</NButton>
        </RouterLink>
        <RouterLink to="/admin/modules/installer">
          <NButton secondary>安装器视图</NButton>
        </RouterLink>
        <NButton secondary :loading="refreshing" @click="load(true)">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <template v-else-if="detail">
        <div class="module-detail-summary">
          <OjosStatCard label="组件" :value="detail.components.length" tone="primary" />
          <OjosStatCard label="依赖" :value="detail.dependencies.length" />
          <OjosStatCard label="被依赖" :value="detail.dependents.length" />
          <OjosStatCard label="权限" :value="detail.permissions.length" />
        </div>

        <OjosSection title="基础信息">
          <NDescriptions bordered :column="2">
            <NDescriptionsItem label="Module ID">{{ detail.module.module_id }}</NDescriptionsItem>
            <NDescriptionsItem label="名称">{{ detail.module.name }}</NDescriptionsItem>
            <NDescriptionsItem label="集合">{{ detail.module.set_id }}</NDescriptionsItem>
            <NDescriptionsItem label="版本">{{ detail.module.version }}</NDescriptionsItem>
            <NDescriptionsItem label="状态">
              <OjosModuleStatusTag :status="detail.module.status" />
            </NDescriptionsItem>
            <NDescriptionsItem label="类型">{{ detail.module.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="说明" :span="2">
              {{ detail.module.description }}
            </NDescriptionsItem>
          </NDescriptions>
        </OjosSection>

        <OjosSection title="注册表详情">
          <NTabs type="line" animated>
            <NTabPane name="dependencies" tab="依赖">
              <EmptyView v-if="detail.dependencies.length === 0" description="暂无依赖" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependencies" :bordered="false" />
            </NTabPane>
            <NTabPane name="dependents" tab="被依赖">
              <EmptyView v-if="detail.dependents.length === 0" description="暂无被依赖模块" />
              <NDataTable v-else :columns="edgeColumns" :data="detail.dependents" :bordered="false" />
            </NTabPane>
            <NTabPane name="components" tab="组件">
              <EmptyView v-if="detail.components.length === 0" description="暂无组件" />
              <NDataTable
                v-else
                :columns="componentColumns"
                :data="detail.components"
                :pagination="{ pageSize: 8 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="permissions" tab="权限">
              <EmptyView v-if="detail.permissions.length === 0" description="暂无权限" />
              <NDataTable
                v-else
                :columns="permissionColumns"
                :data="detail.permissions"
                :pagination="{ pageSize: 10 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="menus" tab="菜单">
              <EmptyView v-if="detail.menus.length === 0" description="暂无菜单" />
              <NDataTable v-else :columns="menuColumns" :data="detail.menus" :bordered="false" />
            </NTabPane>
            <NTabPane name="frontend" tab="前端路由">
              <EmptyView v-if="detail.frontend_routes.length === 0" description="暂无前端路由" />
              <NDataTable
                v-else
                :columns="frontendRouteColumns"
                :data="detail.frontend_routes"
                :pagination="{ pageSize: 10 }"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="gateway" tab="Gateway 路由">
              <EmptyView v-if="detail.gateway_routes.length === 0" description="暂无 Gateway 路由" />
              <NDataTable
                v-else
                :columns="gatewayRouteColumns"
                :data="detail.gateway_routes"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="health" tab="健康检查">
              <EmptyView v-if="detail.health_checks.length === 0" description="暂无健康检查" />
              <NDataTable
                v-else
                :columns="componentColumns"
                :data="detail.health_checks"
                :bordered="false"
              />
            </NTabPane>
            <NTabPane name="installations" tab="安装记录">
              <EmptyView v-if="detail.installations.length === 0" description="暂无安装记录" />
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
