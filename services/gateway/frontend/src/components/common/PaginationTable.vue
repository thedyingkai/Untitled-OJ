<script setup lang="ts">
import { NButton, NDataTable, NPagination, NSpace, type DataTableColumns } from 'naive-ui'

withDefaults(
  defineProps<{
    columns: DataTableColumns<Record<string, unknown>>
    data: Record<string, unknown>[]
    loading?: boolean
    page: number
    pageSize: number
    total: number
    rowKey?: string
    showRefresh?: boolean
  }>(),
  {
    loading: false,
    rowKey: 'id',
    showRefresh: true,
  },
)

const emit = defineEmits<{
  'update:page': [value: number]
  'update:pageSize': [value: number]
  refresh: []
}>()
</script>

<template>
  <div class="pagination-table">
    <NSpace v-if="showRefresh" justify="end" class="table-actions">
      <NButton size="small" @click="emit('refresh')">刷新</NButton>
    </NSpace>
    <NDataTable
      :columns="columns"
      :data="data"
      :loading="loading"
      :row-key="(row) => row[rowKey]"
      :bordered="false"
    />
    <NSpace justify="end" class="table-pagination">
      <NPagination
        :page="page"
        :page-size="pageSize"
        :item-count="total"
        show-size-picker
        :page-sizes="[10, 20, 50, 100]"
        @update:page="emit('update:page', $event)"
        @update:page-size="emit('update:pageSize', $event)"
      />
    </NSpace>
  </div>
</template>
