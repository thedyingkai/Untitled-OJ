<script setup lang="ts">
import { reactive, ref, watch } from 'vue'
import {
  NButton,
  NForm,
  NFormItem,
  NFormItemGi,
  NGrid,
  NInput,
  NInputNumber,
  NSelect,
  NSpace,
  type FormInst,
  type FormRules,
} from 'naive-ui'

import type {
  ProblemDifficulty,
  ProblemFormInput,
  ProblemStatus,
  ProblemType,
  ProblemVisibility,
} from '../../types/problem'

interface ProblemFormModel {
  title: string
  slug: string
  statement: string
  time_limit_ms: number
  memory_limit_mb: number
  problem_type: ProblemType
  visibility: ProblemVisibility
  difficulty: ProblemDifficulty
  tags: string
  status: ProblemStatus
}

const props = withDefaults(
  defineProps<{
    mode: 'create' | 'edit'
    initialValue?: Partial<ProblemFormInput>
    loading?: boolean
  }>(),
  {
    loading: false,
    initialValue: undefined,
  },
)

const emit = defineEmits<{
  submit: [payload: ProblemFormInput]
  cancel: []
}>()

const formRef = ref<FormInst | null>(null)
const model = reactive<ProblemFormModel>({
  title: '',
  slug: '',
  statement: '',
  time_limit_ms: 1000,
  memory_limit_mb: 256,
  problem_type: 'traditional',
  visibility: 'private',
  difficulty: 'medium',
  tags: '',
  status: 'draft',
})

const rules: FormRules = {
  title: [
    {
      required: true,
      validator: (_rule, value: string) => Boolean(value?.trim()) || new Error('题目标题必填'),
      trigger: ['input', 'blur'],
    },
  ],
  slug: [
    {
      validator: (_rule, value: string) => {
        if (!value?.trim()) {
          return true
        }
        return (
          /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value.trim()) ||
          new Error('只能使用小写字母、数字和连字符')
        )
      },
      trigger: ['input', 'blur'],
    },
  ],
  time_limit_ms: [
    {
      required: true,
      validator: (_rule, value: number) =>
        (Number.isFinite(value) && value >= 1 && value <= 600000) ||
        new Error('时间限制必须为 1 到 600000 ms'),
      trigger: ['change', 'blur'],
    },
  ],
  memory_limit_mb: [
    {
      required: true,
      validator: (_rule, value: number) =>
        (Number.isFinite(value) && value >= 1 && value <= 65536) ||
        new Error('内存限制必须为 1 到 65536 MB'),
      trigger: ['change', 'blur'],
    },
  ],
}

const problemTypeOptions = [
  { label: '传统题', value: 'traditional' },
  { label: '交互题', value: 'interactive' },
  { label: '通信题', value: 'communication' },
  { label: '输出题', value: 'output_only' },
  { label: '启发式', value: 'heuristic' },
]

const visibilityOptions = [
  { label: '私有', value: 'private' },
  { label: '公开', value: 'public' },
]

const difficultyOptions = [
  { label: '简单', value: 'easy' },
  { label: '中等', value: 'medium' },
  { label: '困难', value: 'hard' },
]

const statusOptions = [
  { label: '草稿', value: 'draft' },
  { label: '就绪', value: 'ready' },
  { label: '已发布', value: 'published' },
  { label: '已归档', value: 'archived' },
]

watch(
  () => props.initialValue,
  (value) => {
    model.title = value?.title ?? ''
    model.slug = value?.slug ?? ''
    model.statement = value?.statement ?? ''
    model.time_limit_ms = value?.time_limit_ms ?? 1000
    model.memory_limit_mb = value?.memory_limit_mb ?? 256
    model.problem_type = value?.problem_type ?? 'traditional'
    model.visibility = value?.visibility ?? 'private'
    model.difficulty = value?.difficulty ?? 'medium'
    model.tags = value?.tags ?? ''
    model.status = value?.status ?? 'draft'
  },
  { immediate: true, deep: true },
)

async function submit(): Promise<void> {
  await formRef.value?.validate()

  const payload: ProblemFormInput = {
    title: model.title.trim(),
    statement: model.statement.trim(),
    time_limit_ms: model.time_limit_ms,
    memory_limit_mb: model.memory_limit_mb,
    problem_type: model.problem_type,
    visibility: model.visibility,
    difficulty: model.difficulty,
    tags: model.tags.trim(),
  }

  if (props.mode === 'create') {
    payload.slug = model.slug.trim() || undefined
  } else {
    payload.status = model.status
  }

  emit('submit', payload)
}
</script>

<template>
  <NForm ref="formRef" :model="model" :rules="rules" label-placement="top">
    <NGrid :cols="2" :x-gap="16" :y-gap="8" responsive="screen">
      <NFormItemGi label="标题" path="title">
        <NInput v-model:value="model.title" placeholder="A + B" />
      </NFormItemGi>

      <NFormItemGi v-if="mode === 'create'" label="短标识" path="slug">
        <NInput v-model:value="model.slug" placeholder="a-plus-b" />
      </NFormItemGi>

      <NFormItemGi label="可见性" path="visibility">
        <NSelect v-model:value="model.visibility" :options="visibilityOptions" />
      </NFormItemGi>

      <NFormItemGi label="难度" path="difficulty">
        <NSelect v-model:value="model.difficulty" :options="difficultyOptions" />
      </NFormItemGi>

      <NFormItemGi label="题型" path="problem_type">
        <NSelect v-model:value="model.problem_type" :options="problemTypeOptions" />
      </NFormItemGi>

      <NFormItemGi v-if="mode === 'edit'" label="状态" path="status">
        <NSelect v-model:value="model.status" :options="statusOptions" />
      </NFormItemGi>

      <NFormItemGi label="时间限制 (ms)" path="time_limit_ms">
        <NInputNumber
          v-model:value="model.time_limit_ms"
          :min="1"
          :max="600000"
          :step="100"
          style="width: 100%"
        />
      </NFormItemGi>

      <NFormItemGi label="内存限制 (MB)" path="memory_limit_mb">
        <NInputNumber
          v-model:value="model.memory_limit_mb"
          :min="1"
          :max="65536"
          :step="64"
          style="width: 100%"
        />
      </NFormItemGi>
    </NGrid>

    <NFormItem label="标签" path="tags">
      <NInput v-model:value="model.tags" placeholder="math, implementation" />
    </NFormItem>

    <NFormItem label="题面" path="statement">
      <NInput
        v-model:value="model.statement"
        type="textarea"
        placeholder="题目描述、输入输出格式和数据范围"
        :autosize="{ minRows: 10, maxRows: 24 }"
      />
    </NFormItem>

    <NSpace justify="end">
      <NButton :disabled="loading" @click="emit('cancel')">取消</NButton>
      <NButton type="primary" :loading="loading" @click="submit">
        {{ mode === 'create' ? '创建题目' : '保存修改' }}
      </NButton>
    </NSpace>
  </NForm>
</template>
