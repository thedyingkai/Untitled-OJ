<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import {
  NButton,
  NDescriptions,
  NDescriptionsItem,
  NForm,
  NFormItem,
  NSelect,
  NSpace,
  useMessage,
} from 'naive-ui'

import { createSubmission, listJudgeLanguages } from '../../api/judge'
import { getProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import CodeEditor from '../../components/common/CodeEditor.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import type { JudgeLanguage } from '../../types/judge'
import type { ProblemItem } from '../../types/problem'

const route = useRoute()
const router = useRouter()
const message = useMessage()

const loading = ref(false)
const submitting = ref(false)
const error = ref<unknown>()
const problem = ref<ProblemItem | null>(null)
const languages = ref<JudgeLanguage[]>([])
const language = ref('')
const code = ref('')

const problemId = computed(() => Number(route.params.id))
const languageOptions = computed(() =>
  languages.value.map((item) => ({
    label: item.version ? `${item.display_name} (${item.version})` : item.display_name,
    value: item.id,
    disabled: !item.enabled,
  })),
)

const canSubmit = computed(
  () => Boolean(problem.value && language.value && code.value.trim()) && !submitting.value,
)

async function load(): Promise<void> {
  if (!Number.isFinite(problemId.value) || problemId.value <= 0) {
    error.value = new Error('Invalid problem id')
    return
  }

  loading.value = true
  error.value = undefined

  try {
    const [problemData, languageData] = await Promise.all([
      getProblem(problemId.value),
      listJudgeLanguages(),
    ])
    problem.value = problemData.problem
    languages.value = languageData.languages
    language.value = languages.value.find((item) => item.enabled)?.id ?? ''
    if (!code.value) {
      code.value = defaultCode(language.value)
    }
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

async function submit(): Promise<void> {
  if (!canSubmit.value || !problem.value) {
    return
  }

  submitting.value = true
  error.value = undefined

  try {
    const data = await createSubmission({
      problem_id: problem.value.id,
      language: language.value,
      code: code.value,
    })
    message.success('Submission created')
    await router.push(`/submissions/${data.submission_id}`)
  } catch (err) {
    error.value = err
  } finally {
    submitting.value = false
  }
}

function onLanguageChange(value: string): void {
  if (!code.value.trim()) {
    code.value = defaultCode(value)
  }
}

function defaultCode(value: string): string {
  if (value === 'python3') {
    return 'import sys\n\nprint(sum(map(int, sys.stdin.read().split())))\n'
  }
  if (value === 'java17') {
    return 'import java.util.*;\n\npublic class Main {\n    public static void main(String[] args) {\n        Scanner sc = new Scanner(System.in);\n        System.out.println(sc.nextInt() + sc.nextInt());\n    }\n}\n'
  }
  if (value === 'c11') {
    return '#include <stdio.h>\n\nint main(void) {\n    int a, b;\n    if (scanf("%d%d", &a, &b) != 2) return 0;\n    printf("%d\\n", a + b);\n    return 0;\n}\n'
  }
  return '#include <bits/stdc++.h>\nusing namespace std;\n\nint main() {\n    int a, b;\n    if (!(cin >> a >> b)) return 0;\n    cout << a + b << "\\n";\n    return 0;\n}\n'
}

onMounted(() => {
  void load()
})
</script>

<template>
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !problem" />

    <PageCard v-if="problem" :title="`Submit: ${problem.title}`">
      <template #headerExtra>
        <RouterLink :to="`/problems/${problem.id}`">
          <NButton secondary>Back</NButton>
        </RouterLink>
      </template>

      <NSpace vertical size="large">
        <NDescriptions bordered :column="3" label-placement="left">
          <NDescriptionsItem label="Problem">{{ problem.id }}</NDescriptionsItem>
          <NDescriptionsItem label="Time">{{ problem.time_limit_ms }} ms</NDescriptionsItem>
          <NDescriptionsItem label="Memory">{{ problem.memory_limit_mb }} MB</NDescriptionsItem>
        </NDescriptions>

        <NForm label-placement="top">
          <NFormItem label="Language" required>
            <NSelect
              v-model:value="language"
              :options="languageOptions"
              placeholder="Select language"
              @update:value="onLanguageChange"
            />
          </NFormItem>
          <NFormItem label="Code" required>
            <CodeEditor v-model="code" :language="language" :max-length="262144" />
          </NFormItem>
          <NSpace justify="end">
            <NButton secondary :loading="loading" @click="load">Refresh</NButton>
            <NButton type="primary" :disabled="!canSubmit" :loading="submitting" @click="submit">
              Submit
            </NButton>
          </NSpace>
        </NForm>
      </NSpace>
    </PageCard>
  </NSpace>
</template>
