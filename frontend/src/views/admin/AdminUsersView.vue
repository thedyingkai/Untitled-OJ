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
    title: 'Account',
    key: 'username',
    minWidth: 180,
    render: (row) =>
      h('div', { class: 'identity-cell' }, [
        h('strong', row.username),
        h('span', row.email || 'No email'),
      ]),
  },
  {
    title: 'Roles',
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
  { title: 'Created', key: 'created_at', render: (row) => h(TimeText, { value: row.created_at }) },
  {
    title: 'Grant Role',
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
          { default: () => 'Grant' },
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
    message.warning('Select a role first')
    return
  }
  saving.value = true
  try {
    await addUserRole({ user_id: user.user_id, role })
    message.success('Role granted')
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
    message.success('Role removed')
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
      title="Users"
      description="Manage platform users and grant or remove system roles through the real Auth API."
      eyebrow="Admin"
    >
      <template #actions>
        <NButton :loading="saving || loading" secondary @click="load()">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <OjosErrorState v-if="error" :error="error" @retry="load()" />
      <template v-else>
        <div class="admin-summary-grid">
          <OjosStatCard label="Users" :value="users.length" tone="primary" />
          <OjosStatCard label="Admin Accounts" :value="adminUsers" tone="warning" />
          <OjosStatCard label="Available Roles" :value="roles.length" />
        </div>

        <OjosSection title="Role Assignment" description="Grant one role at a time and remove non-default roles from the table.">
          <OjosToolbar>
            <span class="muted-text">Role changes are applied immediately and recorded by the Auth service.</span>
            <template #actions>
              <NButton :loading="saving" secondary @click="load()">Reload users</NButton>
            </template>
          </OjosToolbar>

          <OjosEmptyState v-if="users.length === 0" description="No users" />
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
