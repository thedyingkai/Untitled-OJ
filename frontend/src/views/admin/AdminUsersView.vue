<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { NButton, NDataTable, NSelect, NSpace, NTag, useMessage, type DataTableColumns } from 'naive-ui'

import { addUserRole, listAdminRoles, listAdminUsers, removeUserRole } from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import TimeText from '../../components/common/TimeText.vue'
import type { RoleItem, UserAdminItem } from '../../types/permission'

const message = useMessage()
const users = ref<UserAdminItem[]>([])
const roles = ref<RoleItem[]>([])
const selectedRole = ref<Record<number, string>>({})
const loading = ref(true)
const saving = ref(false)
const error = ref<ApiClientError | null>(null)

const roleOptions = computed(() => roles.value.map((role) => ({ label: role.name, value: role.name })))

const columns = computed<DataTableColumns<UserAdminItem>>(() => [
  { title: 'ID', key: 'user_id', width: 80 },
  { title: 'Username', key: 'username' },
  { title: 'Email', key: 'email' },
  {
    title: 'Roles',
    key: 'roles',
    render: (row) =>
      h(NSpace, { size: 4 }, () =>
        row.roles.map((role) =>
          h(
            NTag,
            { size: 'small', closable: role !== 'user', onClose: () => handleRemove(row, role) },
            { default: () => role },
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
  <PageCard title="Users">
    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />
      <NSpace v-else vertical>
        <NSpace justify="end">
          <NButton :loading="saving" secondary @click="load()">Refresh</NButton>
        </NSpace>
        <EmptyView v-if="users.length === 0" description="No users" />
        <NDataTable v-else :columns="columns" :data="users" :pagination="{ pageSize: 12 }" />
      </NSpace>
    </template>
  </PageCard>
</template>
