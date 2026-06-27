<script setup lang="ts">
import { computed, h, onMounted, reactive, ref } from 'vue'
import {
  NButton,
  NForm,
  NFormItem,
  NInputNumber,
  NSelect,
  NTabs,
  NTabPane,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  addProblemRole,
  listAdminRoles,
  listAuditLogs,
  removeProblemRole,
} from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import { getModuleRuntimeSnapshot } from '../../api/modules'
import LoadingView from '../../components/common/LoadingView.vue'
import TimeText from '../../components/common/TimeText.vue'
import OjosDataTable from '../../components/oj/OjosDataTable.vue'
import OjosEmptyState from '../../components/oj/OjosEmptyState.vue'
import OjosErrorState from '../../components/oj/OjosErrorState.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosPermissionTag from '../../components/oj/OjosPermissionTag.vue'
import OjosRoleTag from '../../components/oj/OjosRoleTag.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'
import type { ModulePermissionItem } from '../../types/module'
import type { AuditLogItem, RoleItem } from '../../types/permission'

const message = useMessage()
const roles = ref<RoleItem[]>([])
const permissions = ref<ModulePermissionItem[]>([])
const auditLogs = ref<AuditLogItem[]>([])
const loading = ref(true)
const saving = ref(false)
const error = ref<ApiClientError | null>(null)
const grantForm = reactive({
  user_id: null as number | null,
  problem_id: null as number | null,
  role: 'problem_owner',
})

const roleOptions = computed(() =>
  roles.value
    .filter((role) => role.name.startsWith('problem_'))
    .map((role) => ({ label: role.name, value: role.name })),
)

const systemRoleCount = computed(() => roles.value.filter((role) => role.is_system).length)
const problemRoleCount = computed(() => roles.value.filter((role) => role.name.startsWith('problem_')).length)

const roleColumns: DataTableColumns<RoleItem> = [
  { title: 'Role', key: 'name', render: (row) => h(OjosRoleTag, { role: row.name }) },
  { title: 'Module', key: 'module_code' },
  { title: 'Description', key: 'description' },
]

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: 'Permission', key: 'permission_key', render: (row) => h(OjosPermissionTag, { permission: row.permission_key }) },
  { title: 'Module', key: 'module_id' },
  { title: 'Description', key: 'description' },
]

const auditColumns: DataTableColumns<AuditLogItem> = [
  { title: 'Action', key: 'action' },
  { title: 'Target', key: 'target', render: (row) => `${row.target_type}:${row.target_id}` },
  { title: 'Role', key: 'role_name' },
  { title: 'Permission', key: 'permission_code' },
  { title: 'Scope', key: 'scope', render: (row) => `${row.scope_type}:${row.scope_id}` },
  { title: 'Actor', key: 'actor', render: (row) => `${row.actor_type}:${row.actor_id}` },
  { title: 'Created', key: 'created_at', render: (row) => h(TimeText, { value: row.created_at }) },
]

async function load(): Promise<void> {
  loading.value = true
  error.value = null
  try {
    const [roleResp, runtimeResp, auditResp] = await Promise.all([
      listAdminRoles(),
      getModuleRuntimeSnapshot(),
      listAuditLogs(),
    ])
    roles.value = roleResp
    permissions.value = runtimeResp.permissions
    auditLogs.value = auditResp
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
  }
}

async function submitGrant(add: boolean): Promise<void> {
  if (!grantForm.user_id || !grantForm.problem_id || !grantForm.role) {
    message.warning('Complete the problem role form')
    return
  }
  const payload = {
    user_id: grantForm.user_id,
    problem_id: grantForm.problem_id,
    role: grantForm.role,
  }
  saving.value = true
  try {
    if (add) {
      await addProblemRole(payload)
      message.success('Problem role granted')
    } else {
      await removeProblemRole(payload)
      message.success('Problem role removed')
    }
    await load()
  } catch (err) {
    message.error(toApiClientError(err).message)
  } finally {
    saving.value = false
  }
}

onMounted(() => void load())
</script>

<template>
  <div class="admin-permissions-page">
    <OjosPageHeader
      title="Permissions"
      description="Review role definitions, permission points, problem-scoped grants, and audit history."
      eyebrow="Admin"
    >
      <template #actions>
        <NButton secondary :loading="loading || saving" @click="load()">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <OjosErrorState v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="admin-summary-grid">
          <OjosStatCard label="Roles" :value="roles.length" tone="primary" />
          <OjosStatCard label="Problem Roles" :value="problemRoleCount" tone="warning" />
          <OjosStatCard label="Permissions" :value="permissions.length" />
          <OjosStatCard label="Audit Logs" :value="auditLogs.length" />
        </div>

        <OjosSection
          title="Problem-scoped Role"
          description="Grant or remove roles bound to one problem. This uses the same Gateway admin API as runtime validation."
        >
          <OjosToolbar>
            <NForm inline :model="grantForm" class="permission-grant-form">
              <NFormItem label="User ID">
                <NInputNumber v-model:value="grantForm.user_id" :min="1" />
              </NFormItem>
              <NFormItem label="Problem ID">
                <NInputNumber v-model:value="grantForm.problem_id" :min="1" />
              </NFormItem>
              <NFormItem label="Role">
                <NSelect v-model:value="grantForm.role" :options="roleOptions" style="width: 190px" />
              </NFormItem>
            </NForm>
            <template #actions>
              <NButton type="primary" :loading="saving" @click="submitGrant(true)">Grant</NButton>
              <NButton secondary :loading="saving" @click="submitGrant(false)">Remove</NButton>
            </template>
          </OjosToolbar>
        </OjosSection>

        <OjosSection
          title="Authorization Registry"
          :description="`${systemRoleCount} system roles, ${problemRoleCount} problem roles, and ${permissions.length} active module permission points.`"
        >
          <NTabs type="line" animated>
            <NTabPane name="roles" tab="Roles">
              <OjosEmptyState v-if="roles.length === 0" description="No roles" />
              <OjosDataTable v-else :columns="roleColumns" :data="roles" :page-size="12" />
            </NTabPane>
            <NTabPane name="permissions" tab="Permissions">
              <OjosEmptyState v-if="permissions.length === 0" description="No permissions" />
              <OjosDataTable v-else :columns="permissionColumns" :data="permissions" :page-size="12" />
            </NTabPane>
            <NTabPane name="audit" tab="Audit Logs">
              <OjosEmptyState v-if="auditLogs.length === 0" description="No audit logs" />
              <OjosDataTable v-else :columns="auditColumns" :data="auditLogs" :page-size="10" />
            </NTabPane>
          </NTabs>
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.admin-permissions-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.admin-summary-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.permission-grant-form {
  width: 100%;
}

@media (max-width: 1000px) {
  .admin-summary-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 680px) {
  .admin-summary-grid {
    grid-template-columns: 1fr;
  }
}
</style>
