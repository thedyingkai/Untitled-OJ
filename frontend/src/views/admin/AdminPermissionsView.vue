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
  { title: '角色', key: 'name', render: (row) => h(OjosRoleTag, { role: row.name }) },
  { title: '模块', key: 'module_code' },
  { title: '说明', key: 'description' },
]

const permissionColumns: DataTableColumns<ModulePermissionItem> = [
  { title: '权限', key: 'permission_key', render: (row) => h(OjosPermissionTag, { permission: row.permission_key }) },
  { title: '模块', key: 'module_id' },
  { title: '说明', key: 'description' },
]

const auditColumns: DataTableColumns<AuditLogItem> = [
  { title: '动作', key: 'action' },
  { title: '目标', key: 'target', render: (row) => `${row.target_type}:${row.target_id}` },
  { title: '角色', key: 'role_name' },
  { title: '权限', key: 'permission_code' },
  { title: '作用域', key: 'scope', render: (row) => `${row.scope_type}:${row.scope_id}` },
  { title: '操作者', key: 'actor', render: (row) => `${row.actor_type}:${row.actor_id}` },
  { title: '创建时间', key: 'created_at', render: (row) => h(TimeText, { value: row.created_at }) },
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
    message.warning('请填写完整的题目角色表单')
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
      message.success('题目角色已授予')
    } else {
      await removeProblemRole(payload)
      message.success('题目角色已移除')
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
      title="权限"
      description="查看角色定义、权限点、题目作用域授权和审计历史。"
      eyebrow="管理"
    >
      <template #actions>
        <NButton secondary :loading="loading || saving" @click="load()">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <OjosErrorState v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="admin-summary-grid">
          <OjosStatCard label="角色" :value="roles.length" tone="primary" />
          <OjosStatCard label="题目角色" :value="problemRoleCount" tone="warning" />
          <OjosStatCard label="权限" :value="permissions.length" />
          <OjosStatCard label="审计日志" :value="auditLogs.length" />
        </div>

        <OjosSection
          title="题目作用域角色"
          description="授予或移除绑定到单个题目的角色；使用与 Runtime 校验一致的 Gateway 管理 API。"
        >
          <OjosToolbar>
            <NForm inline :model="grantForm" class="permission-grant-form">
              <NFormItem label="用户 ID">
                <NInputNumber v-model:value="grantForm.user_id" :min="1" />
              </NFormItem>
              <NFormItem label="题目 ID">
                <NInputNumber v-model:value="grantForm.problem_id" :min="1" />
              </NFormItem>
              <NFormItem label="角色">
                <NSelect v-model:value="grantForm.role" :options="roleOptions" style="width: 190px" />
              </NFormItem>
            </NForm>
            <template #actions>
              <NButton type="primary" :loading="saving" @click="submitGrant(true)">授予</NButton>
              <NButton secondary :loading="saving" @click="submitGrant(false)">移除</NButton>
            </template>
          </OjosToolbar>
        </OjosSection>

        <OjosSection
          title="授权注册表"
          :description="`${systemRoleCount} 个系统角色，${problemRoleCount} 个题目角色，${permissions.length} 个启用的模块权限点。`"
        >
          <NTabs type="line" animated>
            <NTabPane name="roles" tab="角色">
              <OjosEmptyState v-if="roles.length === 0" description="暂无角色" />
              <OjosDataTable v-else :columns="roleColumns" :data="roles" :page-size="12" />
            </NTabPane>
            <NTabPane name="permissions" tab="权限">
              <OjosEmptyState v-if="permissions.length === 0" description="暂无权限" />
              <OjosDataTable v-else :columns="permissionColumns" :data="permissions" :page-size="12" />
            </NTabPane>
            <NTabPane name="audit" tab="审计日志">
              <OjosEmptyState v-if="auditLogs.length === 0" description="暂无审计日志" />
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
