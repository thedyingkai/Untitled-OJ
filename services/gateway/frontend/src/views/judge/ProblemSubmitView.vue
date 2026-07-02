<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import { NButton, NForm, NFormItem, NSelect, NSpace, useMessage } from 'naive-ui'

import { createSubmission, listJudgeLanguages } from '../../api/judge'
import { getProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import CodeEditor from '../../components/common/CodeEditor.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosDifficultyTag from '../../components/oj/OjosDifficultyTag.vue'
import OjosLanguageTag from '../../components/oj/OjosLanguageTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosVisibilityTag from '../../components/oj/OjosVisibilityTag.vue'
import type { JudgeLanguage } from '../../types/judge'
import type { ProblemItem } from '../../types/problem'
import { formatDuration, formatMemoryLimit } from '../../utils/format'

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

const selectedLanguage = computed(() => languages.value.find((item) => item.id === language.value))
const canSubmit = computed(
  () => Boolean(problem.value && language.value && code.value.trim()) && !submitting.value,
)

async function load(): Promise<void> {
  if (!Number.isFinite(problemId.value) || problemId.value <= 0) {
    error.value = new Error('题目 ID 无效')
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
    message.success(`提交 #${data.submission_id} 已创建`)
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
  <div class="submit-page" data-testid="submission-page">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !problem" />
    <EmptyView v-else-if="!loading && !error && !problem" description="未找到题目" />

    <template v-if="problem">
      <OjosPageHeader
        :title="`提交：${problem.title}`"
        :description="`${problem.id} · ${problem.slug}`"
        eyebrow="提交"
      >
        <template #actions>
          <RouterLink :to="`/problems/${problem.id}`">
            <NButton secondary>返回题目</NButton>
          </RouterLink>
          <NButton secondary :loading="loading" @click="load">刷新</NButton>
        </template>
      </OjosPageHeader>

      <div class="submit-layout">
        <main class="submit-editor">
          <OjosSection title="源代码">
            <NForm label-placement="top">
              <NFormItem label="语言" required>
                <NSelect
                  v-model:value="language"
                  :options="languageOptions"
                  placeholder="选择语言"
                  @update:value="onLanguageChange"
                />
              </NFormItem>
              <NFormItem label="代码" required>
                <CodeEditor v-model="code" :language="language" :max-length="262144" />
              </NFormItem>
              <NSpace justify="end">
                <NButton
                  type="primary"
                  :disabled="!canSubmit"
                  :loading="submitting"
                  data-testid="submit-solution"
                  @click="submit"
                >
                  提交
                </NButton>
              </NSpace>
            </NForm>
          </OjosSection>
        </main>

        <aside class="submit-aside">
          <OjosSection title="题目信息">
            <div class="submit-meta-stack">
              <OjosStatCard label="时间限制" :value="formatDuration(problem.time_limit_ms)" />
              <OjosStatCard label="内存限制" :value="formatMemoryLimit(problem.memory_limit_mb)" />
              <div class="submit-tags">
                <OjosDifficultyTag :difficulty="problem.difficulty" />
                <OjosVisibilityTag :visibility="problem.visibility" />
                <OjosLanguageTag
                  v-if="selectedLanguage"
                  :language="selectedLanguage.id"
                  :enabled="selectedLanguage.enabled"
                />
              </div>
            </div>
          </OjosSection>
        </aside>
      </div>
    </template>
  </div>
</template>

<style scoped>
.submit-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.submit-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 300px;
  gap: 18px;
  align-items: start;
}

.submit-aside {
  position: sticky;
  top: 92px;
}

.submit-meta-stack {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.submit-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

@media (max-width: 1100px) {
  .submit-layout {
    grid-template-columns: 1fr;
  }

  .submit-aside {
    position: static;
  }
}
</style>
