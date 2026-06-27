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
  useDialog,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  disableModule,
  discoverModules,
  enableModule,
  getModuleInstallerHealth,
  installModule,
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
  ModuleInstallData,
  ModuleOperationItem,
  ModulePlan,
  ModulePlanAction,
} from '../../types/module'
import { formatDateTime } from '../../utils/format'

const dialog = useDialog()
const message = useMessage()

const manifestPath = ref('modules/demo-module/module.yaml')
const targetModuleId = ref('ojos.demo-module')
const discovered = ref<ModuleDiscoverItem[]>([])
const selectedPlan = ref<ModulePlan | null>(null)
const installResult = ref<ModuleInstallData | null>(null)
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
  { title: 'Module', key: 'module_id', minWidth: 220 },
  { title: 'Name', key: 'name', minWidth: 180 },
  { title: 'Version', key: 'version', width: 110 },
  {
    title: 'Status',
    key: 'status',
    width: 130,
    render: (row) => row.status ? h(OjosModuleStatusTag, { status: row.status }) : h(NTag, { size: 'small' }, { default: () => row.valid === false ? 'invalid' : 'valid' }),
  },
  { title: 'Error', key: 'error', minWidth: 220 },
]

const actionColumns: DataTableColumns<ModulePlanAction> = [
  { title: 'Action', key: 'action', minWidth: 200 },
  { title: 'Target', key: 'target', minWidth: 220 },
  { title: 'Detail', key: 'detail', minWidth: 260 },
]

const operationColumns: DataTableColumns<ModuleOperationItem> = [
  { title: 'Operation', key: 'operation_id', minWidth: 260 },
  { title: 'Action', key: 'action', width: 120 },
  { title: 'Status', key: 'status', width: 130, render: (row) => h(OjosModuleStatusTag, { status: row.status }) },
  { title: 'Actor', key: 'actor_username', width: 140 },
  { title: 'Updated', key: 'updated_at', width: 180, render: (row) => formatDateTime(row.updated_at) },
  { title: 'Error', key: 'error_message', minWidth: 180 },
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
  }, 'Module discovery refreshed')
  loading.value = false
}

async function validateCurrent(): Promise<void> {
  await guarded(async () => {
    await validateModule({ manifest_path: manifestPath.value })
  }, 'Manifest validated')
}

async function planCurrent(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await planModule({ manifest_path: manifestPath.value, dry_run: true })
  }, 'Install plan generated')
}

function confirmApplyInstall(): void {
  dialog.warning({
    title: 'Apply module install',
    content: 'This writes module registry metadata and operation history. Review the plan before applying.',
    positiveText: 'Apply',
    negativeText: 'Cancel',
    onPositiveClick: () => void applyInstall(),
  })
}

async function applyInstall(): Promise<void> {
  await guarded(async () => {
    installResult.value = await installModule({ manifest_path: manifestPath.value, dry_run: false })
    selectedPlan.value = installResult.value.plan
    targetModuleId.value = selectedPlan.value.module_id
    await refreshOperations()
  }, 'Module install applied')
}

async function applyEnable(): Promise<void> {
  await guarded(async () => {
    installResult.value = await enableModule(targetModuleId.value)
    selectedPlan.value = installResult.value.plan
    await refreshOperations()
  }, 'Module enabled')
}

async function applyDisable(): Promise<void> {
  if (protectedTarget.value) {
    message.warning('Protected modules cannot be disabled from Installer v0')
    return
  }
  dialog.warning({
    title: 'Disable module',
    content: 'Disabling a module can hide routes, menus, and metadata. Enabled dependents will block this operation.',
    positiveText: 'Disable',
    negativeText: 'Cancel',
    onPositiveClick: () => void guarded(async () => {
      installResult.value = await disableModule(targetModuleId.value)
      selectedPlan.value = installResult.value.plan
      await refreshOperations()
    }, 'Module disabled'),
  })
}

async function buildUpgradePlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await upgradePlanModule(targetModuleId.value, { manifest_path: manifestPath.value, dry_run: true })
  }, 'Upgrade plan generated')
}

async function buildRollbackPlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await rollbackPlanModule(targetModuleId.value)
  }, 'Rollback plan generated')
}

async function buildUninstallPlan(): Promise<void> {
  await guarded(async () => {
    selectedPlan.value = await uninstallDryRunModule(targetModuleId.value)
  }, 'Uninstall dry-run generated')
}

async function refreshHealth(): Promise<void> {
  await guarded(async () => {
    health.value = await getModuleInstallerHealth(targetModuleId.value)
  }, 'Module health refreshed')
}

async function refreshOperations(): Promise<void> {
  await guarded(async () => {
    const resp = await listModuleOperations(targetModuleId.value)
    operations.value = resp.operations
  }, 'Operation history refreshed')
}

onMounted(() => void load())
</script>

<template>
  <div class="installer-page">
    <OjosPageHeader
      title="Module Installer"
      description="Validate local manifests, generate plans, apply metadata installs, and inspect module lifecycle operations."
      eyebrow="Admin"
    >
      <template #actions>
        <NButton secondary :loading="busy" @click="load">Discover</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load" />

      <NAlert type="warning" :show-icon="true">
        Installer v0 supports local manifests and local packages only. It does not execute hooks, load dynamic bundles, or install remote marketplace modules.
      </NAlert>

      <OjosSection title="Local Manifest">
        <div class="installer-controls">
          <NInput v-model:value="manifestPath" placeholder="modules/demo-module/module.yaml" />
          <NInput v-model:value="targetModuleId" placeholder="ojos.demo-module" />
        </div>
        <NSpace>
          <NButton secondary :loading="busy" @click="validateCurrent">Validate</NButton>
          <NButton secondary :loading="busy" @click="planCurrent">Install Dry-run</NButton>
          <NButton type="primary" :loading="busy" @click="confirmApplyInstall">Install Apply</NButton>
          <NButton secondary :loading="busy" @click="applyEnable">Enable</NButton>
          <NButton secondary :disabled="protectedTarget" :loading="busy" @click="applyDisable">Disable</NButton>
        </NSpace>
      </OjosSection>

      <OjosSection title="Planning">
        <NSpace>
          <NButton secondary :loading="busy" @click="buildUpgradePlan">Upgrade Plan</NButton>
          <NButton secondary :loading="busy" @click="buildRollbackPlan">Rollback Plan</NButton>
          <NButton secondary :loading="busy" @click="buildUninstallPlan">Uninstall Dry-run</NButton>
          <NButton secondary :loading="busy" @click="refreshHealth">Health</NButton>
          <NButton secondary :loading="busy" @click="refreshOperations">Operations</NButton>
        </NSpace>
        <NAlert v-if="protectedTarget" type="info" :show-icon="true" class="installer-hint">
          Kernel modules and judge-core are protected. Disable and uninstall apply operations are blocked by Installer v0.
        </NAlert>
      </OjosSection>

      <OjosSection title="Discovered Modules">
        <EmptyView v-if="discovered.length === 0" description="No local module manifests discovered" />
        <NDataTable v-else :columns="discoverColumns" :data="discovered" :pagination="{ pageSize: 8 }" :bordered="false" />
      </OjosSection>

      <OjosSection title="Plan Result">
        <EmptyView v-if="!selectedPlan" description="Generate a plan to inspect affected modules and actions" />
        <template v-else>
          <NDescriptions bordered :column="3">
            <NDescriptionsItem label="Kind">{{ selectedPlan.kind }}</NDescriptionsItem>
            <NDescriptionsItem label="Module">{{ selectedPlan.module_id }}</NDescriptionsItem>
            <NDescriptionsItem label="Version">{{ selectedPlan.version }}</NDescriptionsItem>
            <NDescriptionsItem label="Dry-run">{{ selectedPlan.dry_run ? 'yes' : 'no' }}</NDescriptionsItem>
            <NDescriptionsItem label="Can Apply">
              <NTag :type="selectedPlan.can_apply ? 'success' : 'error'">{{ selectedPlan.can_apply ? 'yes' : 'blocked' }}</NTag>
            </NDescriptionsItem>
            <NDescriptionsItem label="Dependencies">{{ selectedPlan.dependencies.join(', ') || 'none' }}</NDescriptionsItem>
          </NDescriptions>
          <NTabs type="line" class="plan-tabs">
            <NTabPane name="actions" tab="Actions">
              <NDataTable :columns="actionColumns" :data="selectedPlan.actions" :bordered="false" />
            </NTabPane>
            <NTabPane name="blocked" tab="Blocked By">
              <EmptyView v-if="selectedPlan.blocked_by.length === 0" description="No blockers" />
              <div v-else class="pill-list">
                <NTag v-for="item in selectedPlan.blocked_by" :key="item" type="error">{{ item }}</NTag>
              </div>
            </NTabPane>
            <NTabPane name="tables" tab="Affected Tables">
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

      <OjosSection title="Health And Operations">
        <NDescriptions v-if="health" bordered :column="3">
          <NDescriptionsItem label="Module">{{ health.module_id }}</NDescriptionsItem>
          <NDescriptionsItem label="Health">{{ health.status }}</NDescriptionsItem>
          <NDescriptionsItem label="Module Status">
            <OjosModuleStatusTag :status="health.module_status" />
          </NDescriptionsItem>
        </NDescriptions>
        <EmptyView v-if="operations.length === 0" description="No operation history loaded" />
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
