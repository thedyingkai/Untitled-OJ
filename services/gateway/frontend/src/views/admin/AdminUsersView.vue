<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { NButton, NSelect, NSpace, useMessage, type DataTableColumns } from 'naive-ui'

import { addUserRole, listAdminRoles, listAdminUsers, removeUserRole } from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import LoadingView from '../../components/common/LoadingView.vue'
import TimeText from '../../components/common/TimeText.vue'
import OjosDataTable from '../../components/oj/OjosDataTable.vue'
import OjosEmptyState from '../../components/oj/OjosEmptyState.vue'
import OjosErrorState from '../../components/oj/OjosErrorState.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosRoleTag from '../../components/oj/OjosRoleTag.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'
import type { RoleItem, UserAdminItem } from '../../types/permission'

const message = useMessage()
const users = ref<UserAdminItem[]>([])
const roles = ref<RoleItem[]>([])
const selectedRole = ref<Record<number, string>>({})
const loading = ref(true)
const saving = ref(false)
const error = ref<ApiClientError | null>(null)

const roleOptions = computed(() => roles.value.map((role) => ({ label: role.name, value: role.name })))
const adminUsers = computed(() =>
  users.value.filter((user) => user.roles.some((role) => role === 'admin' || role === 'super_admin')).length,
)

const columns = computed<DataTableColumns<UserAdminItem>>(() => [
  { title: 'ID', key: 'user_id', width: 80 },
  {
    title: '账号',
    key: 'username',
    minWidth: 180,
    render: (row) =>
      h('div', { class: 'identity-cell' }, [
        h('strong', row.username),
        h('span', row.email || '无邮箱'),
      ]),
  },
  {
    title: '角色',
    key: 'roles',
    render: (row) =>
      h(NSpace, { size: 4 }, () =>
        row.roles.map((role) =>
          h(
            OjosRoleTag,
            { role, closable: role !== 'user', onClose: () => handleRemove(row, role) },
          ),
        ),
      ),
  },
  { title: '创建时间', key: 'created_at', render: (row) => h(TimeText, { value: row.created_at }) },
  {
    title: '授予角色',
    key: 'grant',
    render: (row) =>
      h(NSpace, { align: 'center' }, () => [
        h(NSelect, {
          value: selectedRole.value[row.user_id],
          options: roleOptions.value,
          size: 'small',
          style: 'width: 180px',
          'onUpdate:value': (value: string) => {
            selectedRole.value[row.user_id] = value
          },
        }),
        h(
          NButton,
          { size: 'small', secondary: true, onClick: () => handleGrant(row) },
          { default: () => '授予' },
        ),
      ]),
  },
])

async function load(): Promise<void> {
  loading.value = true
  error.value = null
  try {
    const [userResp, roleResp] = await Promise.all([listAdminUsers(), listAdminRoles()])
    users.value = userResp
    roles.value = roleResp
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
  }
}

async function handleGrant(user: UserAdminItem): Promise<void> {
  const role = selectedRole.value[user.user_id]
  if (!role) {
    message.warning('请先选择角色')
    return
  }
  saving.value = true
  try {
    await addUserRole({ user_id: user.user_id, role })
    message.success('角色已授予')
    await load()
  } catch (err) {
    message.error(toApiClientError(err).message)
  } finally {
    saving.value = false
  }
}

async function handleRemove(user: UserAdminItem, role: string): Promise<void> {
  saving.value = true
  try {
    await removeUserRole({ user_id: user.user_id, role })
    message.success('角色已移除')
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
  <div class="admin-users-page">
    <OjosPageHeader
      title="用户"
      description="通过真实 Auth API 管理平台用户，并授予或移除系统角色。"
      eyebrow="管理"
    >
      <template #actions>
        <NButton :loading="saving || loading" secondary @click="load()">刷新</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <OjosErrorState v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="admin-summary-grid">
          <OjosStatCard label="用户" :value="users.length" tone="primary" />
          <OjosStatCard label="管理员账号" :value="adminUsers" tone="warning" />
          <OjosStatCard label="可用角色" :value="roles.length" />
        </div>

        <OjosSection title="角色分配" description="每次授予一个角色，可在表格中移除非默认角色。">
          <OjosToolbar>
            <span class="muted-text">角色变更会立即生效，并由 Auth 服务记录。</span>
            <template #actions>
              <NButton :loading="saving" secondary @click="load()">重载用户</NButton>
            </template>
          </OjosToolbar>

          <OjosEmptyState v-if="users.length === 0" description="暂无用户" />
          <OjosDataTable v-else :columns="columns" :data="users" :page-size="12" />
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.admin-users-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.admin-summary-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.identity-cell {
  display: flex;
  min-width: 0;
  flex-direction: column;
}

.identity-cell strong {
  color: var(--text-strong);
}

.identity-cell span,
.muted-text {
  color: var(--muted);
  font-size: 12px;
}

@media (max-width: 900px) {
  .admin-summary-grid {
    grid-template-columns: 1fr;
  }
}
</style>
