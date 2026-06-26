<script setup lang="ts">
import { computed, h, onMounted, reactive, ref } from 'vue'
import {
  NButton,
  NDataTable,
  NForm,
  NFormItem,
  NInputNumber,
  NSelect,
  NSpace,
  NTabs,
  NTabPane,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  addProblemRole,
  listAdminPermissions,
  listAdminRoles,
  listAuditLogs,
  removeProblemRole,
} from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import TimeText from '../../components/common/TimeText.vue'
import type { AuditLogItem, PermissionItem, RoleItem } from '../../types/permission'

const message = useMessage()
const roles = ref<RoleItem[]>([])
const permissions = ref<PermissionItem[]>([])
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

const roleColumns: DataTableColumns<RoleItem> = [
  { title: 'Role', key: 'name' },
  { title: 'Module', key: 'module_code' },
  { title: 'Description', key: 'description' },
]

const permissionColumns: DataTableColumns<PermissionItem> = [
  { title: 'Permission', key: 'code' },
  { title: 'Module', key: 'module_code' },
  { title: 'Name', key: 'name' },
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
    const [roleResp, permissionResp, auditResp] = await Promise.all([
      listAdminRoles(),
      listAdminPermissions(),
      listAuditLogs(),
    ])
    roles.value = roleResp
    permissions.value = permissionResp
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
  <PageCard title="Permissions">
    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NSpace v-else vertical size="large">
        <NForm inline :model="grantForm">
          <NFormItem label="User ID">
            <NInputNumber v-model:value="grantForm.user_id" :min="1" />
          </NFormItem>
          <NFormItem label="Problem ID">
            <NInputNumber v-model:value="grantForm.problem_id" :min="1" />
          </NFormItem>
          <NFormItem label="Role">
            <NSelect v-model:value="grantForm.role" :options="roleOptions" style="width: 180px" />
          </NFormItem>
          <NFormItem>
            <NSpace>
              <NButton type="primary" :loading="saving" @click="submitGrant(true)">Grant</NButton>
              <NButton secondary :loading="saving" @click="submitGrant(false)">Remove</NButton>
              <NButton secondary @click="load()">Refresh</NButton>
            </NSpace>
          </NFormItem>
        </NForm>

        <NTabs type="line">
          <NTabPane name="roles" tab="Roles">
            <EmptyView v-if="roles.length === 0" description="No roles" />
            <NDataTable v-else :columns="roleColumns" :data="roles" :pagination="{ pageSize: 12 }" />
          </NTabPane>
          <NTabPane name="permissions" tab="Permissions">
            <EmptyView v-if="permissions.length === 0" description="No permissions" />
            <NDataTable
              v-else
              :columns="permissionColumns"
              :data="permissions"
              :pagination="{ pageSize: 12 }"
            />
          </NTabPane>
          <NTabPane name="audit" tab="Audit">
            <EmptyView v-if="auditLogs.length === 0" description="No audit logs" />
            <NDataTable
              v-else
              :columns="auditColumns"
              :data="auditLogs"
              :pagination="{ pageSize: 10 }"
            />
          </NTabPane>
        </NTabs>
      </NSpace>
    </template>
  </PageCard>
</template>
