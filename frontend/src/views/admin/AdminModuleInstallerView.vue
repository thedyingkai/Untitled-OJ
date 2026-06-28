<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import {
  NAlert,
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NInput,
  NSpace,
  NTabPane,
  NTabs,
  NTag,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  discoverModules,
  getModuleInstallerHealth,
  listModuleOperations,
  planModule,
  rollbackPlanModule,
  uninstallDryRunModule,
  upgradePlanModule,
  validateModule,
} from '../../api/modules'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosModuleStatusTag from '../../components/oj/OjosModuleStatusTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import type {
  ModuleDiscoverItem,
  ModuleHealthData,
  ModuleOperationItem,
  ModulePlan,
  ModulePlanAction,
} from '../../types/module'
import { formatDateTime } from '../../utils/format'

const message = useMessage()

const manifestPath = ref('modules/demo-module/module.yaml')
const targetModuleId = ref('ojos.demo-module')
const discovered = ref<ModuleDiscoverItem[]>([])
const selectedPlan = ref<ModulePlan | null>(null)
const health = ref<ModuleHealthData | null>(null)
const operations = ref<ModuleOperationItem[]>([])
const loading = ref(true)
const busy = ref(false)
const error = ref<ApiClientError | null>(null)

const protectedTarget = computed(
  () => targetModuleId.value.startsWith('ojos.kernel.') || targetModuleId.value === 'ojos.judge-core',
)

const discoverColumns: DataTableColumns<ModuleDiscoverItem> = [
  { title: 'Manifest', key: 'manifest_path', minWidth: 260 },
  { title: '模块', key: 'module_id', minWidth: 220 },
  { title: '名称', key: 'name', minWidth: 180 },
  { title: '版本', key: 'version', width: 110 },
  {
    title: '状态',
    key: 'status',
    width: 130,
    render: (row) => row.status ? h(OjosModuleStatusTag, { status: row.status }) : h(NTag, { size: 'small' }, { default: () => row.valid === false ? 'invalid' : 'valid' }),
  },
  { title: '错误', key: 'error', minWidth: 220 },
]

const actionColumns: DataTableColumns<ModulePlanAction> = [
  { title: '动作', key: 'action', minWidth: 200 },
  { title: '目标', key: 'target', minWidth: 220 },
  { title: '详情', key: 'detail', minWidth: 260 },
]

const operationColumns: DataTableColumns<ModuleOperationItem> = [
  { title: '操作', key: 'operation_id', minWidth: 260 },
  { title: '动作', key: 'action', width: 120 },
  { title: '状态', key: 'status', width: 130, render: (row) => h(OjosModuleStatusTag, { status: row.status }) },
  { title: '操作者', key: 'actor_username', width: 140 },
  { title: '更新时间', key: 'updated_at', width: 180, render: (row) => formatDateTime(row.updated_at) },
  { title: '错误', key: 'error_message', minWidth: 180 },
]

async function guarded(action: () => Promise<void>, okText: string): Promise<void> {
  busy.value = true
  error.value = null
  try {
    await action()
    message.success(okText)
  } catch (err) {
    error.value = toApiClientError(err)
    message.error(error.value.message)
  } finally {
    busy.value = false
  }
}

async function load(): Promise<void> {
  loading.value = true
  await guarded(async () => {
    const resp = await discoverModules()
    discovered.value = resp.modules
  }, '模块发现已刷新')
  loading.value = false
}

async function validateCurrent(): Promise<void> {
  await guarded(async () => {
    await validateModule({ manifest_path: manifestPath.value })
  }, 'Manifest 校验通过')
}

async function planCurrent(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await planModule({ manifest_path: manifestPath.value, dry_run: true })
  }, '安装 dry-run 计划已生成')
}

async function buildUpgradePlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await upgradePlanModule(targetModuleId.value, { manifest_path: manifestPath.value, dry_run: true })
  }, '升级计划已生成')
}

async function buildRollbackPlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await rollbackPlanModule(targetModuleId.value)
  }, '回滚计划已生成')
}

async function buildUninstallPlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await uninstallDryRunModule(targetModuleId.value)
  }, '卸载 dry-run 计划已生成')
}

async function refreshHealth(): Promise<void> {
  await guarded(async () => {
    health.value = await getModuleInstallerHealth(targetModuleId.value)
  }, '模块健康状态已刷新')
}

async function refreshOperations(): Promise<void> {
  await guarded(async () => {
    const resp = await listModuleOperations(targetModuleId.value)
    operations.value = resp.operations
  }, '操作历史已刷新')
}

onMounted(() => void load())
</script>

<template>
  <div class="installer-page">
    <OjosPageHeader
      title="安装器管理视图"
      description="浏览器端仅作为安装器管理视图。正式安装、启用、禁用、打包和 runtime apply 请使用 ojosctl 或 ojos-installer-tui。"
      eyebrow="管理"
    >
      <template #actions>
        <NButton secondary :loading="busy" @click="load">发现模块</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load" />

      <NAlert type="warning" :show-icon="true">
        官方安装入口是原生 CLI / TUI。Web Shell 不执行安装 apply、启用、禁用或 runtime apply，也不加载 hook、动态前端 bundle 或远程市场模块。
      </NAlert>

      <OjosSection title="本地 Manifest">
        <div class="installer-controls">
          <NInput v-model:value="manifestPath" placeholder="modules/demo-module/module.yaml" />
          <NInput v-model:value="targetModuleId" placeholder="ojos.demo-module" />
        </div>
        <NSpace>
          <NButton secondary :loading="busy" @click="validateCurrent">校验</NButton>
          <NButton secondary :loading="busy" @click="planCurrent">安装 Dry-run</NButton>
        </NSpace>
      </OjosSection>

      <OjosSection title="计划查看">
        <NSpace>
          <NButton secondary :loading="busy" @click="buildUpgradePlan">升级计划</NButton>
          <NButton secondary :loading="busy" @click="buildRollbackPlan">回滚计划</NButton>
          <NButton secondary :loading="busy" @click="buildUninstallPlan">卸载 Dry-run</NButton>
          <NButton secondary :loading="busy" @click="refreshHealth">健康</NButton>
          <NButton secondary :loading="busy" @click="refreshOperations">操作历史</NButton>
        </NSpace>
        <NAlert v-if="protectedTarget" type="info" :show-icon="true" class="installer-hint">
          Kernel modules and judge-core 受保护。禁用和卸载 apply 必须由受控原生安装器处理，Web 只展示计划和状态。
        </NAlert>
      </OjosSection>

      <OjosSection title="已发现模块">
        <EmptyView v-if="discovered.length === 0" description="未发现本地 module.yaml" />
        <NDataTable v-else :columns="discoverColumns" :data="discovered" :pagination="{ pageSize: 8 }" :bordered="false" />
      </OjosSection>

      <OjosSection title="计划结果">
        <EmptyView v-if="!selectedPlan" description="生成计划后可查看影响模块、阻断原因和动作列表" />
        <template v-else>
          <NDescriptions bordered :column="3">
            <NDescriptionsItem label="类型">{{ selectedPlan.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="模块">{{ selectedPlan.module_id }}</NDescriptionsItem>
            <NDescriptionsItem label="版本">{{ selectedPlan.version }}</NDescriptionsItem>
            <NDescriptionsItem label="Dry-run">{{ selectedPlan.dry_run ? '是' : '否' }}</NDescriptionsItem>
            <NDescriptionsItem label="可执行">
              <NTag :type="selectedPlan.can_apply ? 'success' : 'error'">{{ selectedPlan.can_apply ? '允许' : '阻断' }}</NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="依赖">{{ selectedPlan.dependencies.join(', ') || '无' }}</NDescriptionsItem>
          </NDescriptions>
          <NTabs type="line" class="plan-tabs">
            <NTabPane name="actions" tab="动作">
              <NDataTable :columns="actionColumns" :data="selectedPlan.actions" :bordered="false" />
            </NTabPane>
            <NTabPane name="blocked" tab="阻断">
              <EmptyView v-if="selectedPlan.blocked_by.length === 0" description="无阻断" />
              <div v-else class="pill-list">
                <NTag v-for="item in selectedPlan.blocked_by" :key="item" type="error">{{ item }}</NTag>
              </div>
            </NTabPane>
            <NTabPane name="tables" tab="影响表">
              <div class="pill-list">
                <NTag v-for="item in selectedPlan.affected_tables" :key="item">{{ item }}</NTag>
              </div>
            </NTabPane>
            <NTabPane name="json" tab="JSON">
              <OjosJsonViewer :value="selectedPlan" />
            </NTabPane>
          </NTabs>
        </template>
      </OjosSection>

      <OjosSection title="健康与操作历史">
        <NDescriptions v-if="health" bordered :column="3">
          <NDescriptionsItem label="模块">{{ health.module_id }}</NDescriptionsItem>
          <NDescriptionsItem label="健康">{{ health.status }}</NDescriptionsItem>
          <NDescriptionsItem label="模块状态">
            <OjosModuleStatusTag :status="health.module_status" />
          </NDescriptionsItem>
        </NDescriptions>
        <EmptyView v-if="operations.length === 0" description="暂无操作历史" />
        <NDataTable v-else :columns="operationColumns" :data="operations" :pagination="{ pageSize: 8 }" :bordered="false" />
      </OjosSection>
    </template>
  </div>
</template>

<style scoped>
.installer-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.installer-controls {
  display: grid;
  grid-template-columns: minmax(0, 2fr) minmax(0, 1fr);
  gap: 12px;
  margin-bottom: 12px;
}

.installer-hint,
.plan-tabs {
  margin-top: 12px;
}

.pill-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

@media (max-width: 800px) {
  .installer-controls {
    grid-template-columns: 1fr;
  }
}
</style>
