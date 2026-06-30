<script setup lang="ts" generic="T extends object">
import { NDataTable, type DataTableColumns } from 'naive-ui'

withDefaults(
  defineProps<{
    columns: DataTableColumns<T>
    data: T[]
    loading?: boolean
    pageSize?: number
    striped?: boolean
  }>(),
  {
    loading: false,
    pageSize: 0,
    striped: false,
  },
)
</script>

<template>
  <NDataTable
    class="ojos-data-table"
    :columns="columns"
    :data="data"
    :loading="loading"
    :bordered="false"
    :striped="striped"
    :pagination="pageSize > 0 ? { pageSize } : false"
  />
</template>
