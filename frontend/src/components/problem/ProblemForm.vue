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
      validator: (_rule, value: string) => Boolean(value?.trim()) || new Error('Title is required'),
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
          new Error('Use lowercase letters, numbers, and hyphen')
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
        new Error('Time limit must be 1..600000 ms'),
      trigger: ['change', 'blur'],
    },
  ],
  memory_limit_mb: [
    {
      required: true,
      validator: (_rule, value: number) =>
        (Number.isFinite(value) && value >= 1 && value <= 65536) ||
        new Error('Memory limit must be 1..65536 MB'),
      trigger: ['change', 'blur'],
    },
  ],
}

const problemTypeOptions = [
  { label: 'Traditional', value: 'traditional' },
  { label: 'Interactive', value: 'interactive' },
  { label: 'Communication', value: 'communication' },
  { label: 'Output only', value: 'output_only' },
  { label: 'Heuristic', value: 'heuristic' },
]

const visibilityOptions = [
  { label: 'Private', value: 'private' },
  { label: 'Public', value: 'public' },
  { label: 'Contest only', value: 'contest_only' },
]

const difficultyOptions = [
  { label: 'Easy', value: 'easy' },
  { label: 'Medium', value: 'medium' },
  { label: 'Hard', value: 'hard' },
]

const statusOptions = [
  { label: 'Draft', value: 'draft' },
  { label: 'Ready', value: 'ready' },
  { label: 'Published', value: 'published' },
  { label: 'Archived', value: 'archived' },
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
      <NFormItemGi label="Title" path="title">
        <NInput v-model:value="model.title" placeholder="A + B Problem" />
      </NFormItemGi>

      <NFormItemGi v-if="mode === 'create'" label="Slug" path="slug">
        <NInput v-model:value="model.slug" placeholder="a-plus-b" />
      </NFormItemGi>

      <NFormItemGi label="Visibility" path="visibility">
        <NSelect v-model:value="model.visibility" :options="visibilityOptions" />
      </NFormItemGi>

      <NFormItemGi label="Difficulty" path="difficulty">
        <NSelect v-model:value="model.difficulty" :options="difficultyOptions" />
      </NFormItemGi>

      <NFormItemGi label="Problem type" path="problem_type">
        <NSelect v-model:value="model.problem_type" :options="problemTypeOptions" />
      </NFormItemGi>

      <NFormItemGi v-if="mode === 'edit'" label="Status" path="status">
        <NSelect v-model:value="model.status" :options="statusOptions" />
      </NFormItemGi>

      <NFormItemGi label="Time limit (ms)" path="time_limit_ms">
        <NInputNumber
          v-model:value="model.time_limit_ms"
          :min="1"
          :max="600000"
          :step="100"
          style="width: 100%"
        />
      </NFormItemGi>

      <NFormItemGi label="Memory limit (MB)" path="memory_limit_mb">
        <NInputNumber
          v-model:value="model.memory_limit_mb"
          :min="1"
          :max="65536"
          :step="64"
          style="width: 100%"
        />
      </NFormItemGi>
    </NGrid>

    <NFormItem label="Tags" path="tags">
      <NInput v-model:value="model.tags" placeholder="math, implementation" />
    </NFormItem>

    <NFormItem label="Statement" path="statement">
      <NInput
        v-model:value="model.statement"
        type="textarea"
        placeholder="Problem statement, input/output description, and constraints"
        :autosize="{ minRows: 10, maxRows: 24 }"
      />
    </NFormItem>

    <NSpace justify="end">
      <NButton :disabled="loading" @click="emit('cancel')">Cancel</NButton>
      <NButton type="primary" :loading="loading" @click="submit">
        {{ mode === 'create' ? 'Create problem' : 'Save changes' }}
      </NButton>
    </NSpace>
  </NForm>
</template>
