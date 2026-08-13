import { expect, test, type Page, type Route } from '@playwright/test'

const now = '2026-07-03T00:00:00Z'

const problem = {
  id: 1,
  problem_no: 'P1001',
  slug: 'a-plus-b',
  title: 'A+B Problem',
  statement: 'Read two integers and print their sum.',
  statement_format: 'markdown+latex',
  solution: '',
  solution_format: 'markdown+latex',
  problem_type: 'traditional',
  visibility: 'public',
  status: 'published',
  difficulty: 'easy',
  tags: 'math, smoke',
  time_limit_ms: 1000,
  memory_limit_mb: 128,
  language_limits: [],
  created_by: 1,
  created_at: now,
  updated_at: now,
  samples: [{ case_no: 1, input: '1 2\n', output: '3\n' }],
}

const submission = {
  id: 5001,
  problem_id: 1,
  user_id: 7,
  language: 'cpp17',
  status: 'ACCEPTED',
  score: 100,
  time_ms: 12,
  memory_kb: 2048,
  message: 'Accepted',
  code_sha256: 'abc123',
  created_at: now,
  updated_at: now,
  judged_at: now,
}

async function fulfillJson(route: Route, body: unknown, status = 200): Promise<void> {
  await route.fulfill({
    status,
    contentType: 'application/json',
    body: JSON.stringify(body),
  })
}

async function mockApi(page: Page): Promise<void> {
  await page.route('**/api/v1/contributions/snapshot', async (route) => {
    await fulfillJson(route, {
      schema_version: 'ojos.dev/contribution-snapshot/v1',
      digest: `sha256:${'0'.repeat(64)}`,
      generated_at_ms: 1,
      scope_id: 'default',
      ack_obligation: null,
      revisions: [],
      api_surfaces: [],
      gateway_routes: [],
      permission_definitions: [],
      frontend_modules: [],
    })
  })

  await page.route('**/api/auth/login', async (route) => {
    const body = route.request().postDataJSON() as { username?: string; password?: string }
    if (body.username === 'operator' && body.password === 'correct-password') {
      await fulfillJson(route, {
        token: 'e2e-token',
        user_id: 7,
        username: 'operator',
        roles: ['user'],
        permissions: ['judge.submit', 'judge.submission.view.own', 'problem.read'],
      })
      return
    }
    await fulfillJson(route, { message: 'Invalid credentials' }, 401)
  })

  await page.route('**/api/auth/profile', async (route) => {
    await fulfillJson(route, {
      user_id: 7,
      username: 'operator',
      roles: ['user'],
      permissions: ['judge.submit', 'judge.submission.view.own', 'problem.read'],
    })
  })

  await page.route('**/api/health', async (route) => {
    await fulfillJson(route, { status: 'ok', services: [] })
  })

  await page.route((url) => url.pathname === '/api/problem/problems/1', async (route) => {
    await fulfillJson(route, { problem })
  })

  await page.route((url) => url.pathname === '/api/problem/problems', async (route) => {
    await fulfillJson(route, { problems: [problem], total: 1 })
  })

  await page.route('**/api/judge/languages', async (route) => {
    await fulfillJson(route, {
      languages: [
        { id: 'cpp17', display_name: 'C++17', version: 'g++ 12', enabled: true },
        { id: 'python3', display_name: 'Python 3', version: '3.11', enabled: true },
      ],
    })
  })

  await page.route((url) => url.pathname === '/api/judge/submissions/5001/cases', async (route) => {
    await fulfillJson(route, {
      cases: [
        {
          case_no: 1,
          status: 'ACCEPTED',
          score: 100,
          time_ms: 12,
          memory_kb: 2048,
          message: 'ok',
        },
      ],
    })
  })

  await page.route((url) => url.pathname === '/api/judge/submissions/5001', async (route) => {
    await fulfillJson(route, submission)
  })

  await page.route((url) => url.pathname === '/api/judge/submissions', async (route) => {
    if (route.request().method() === 'POST') {
      await fulfillJson(route, { submission_id: 5001, status: 'PENDING' })
      return
    }
    await fulfillJson(route, { submissions: [submission], total: 1 })
  })
}

test.beforeEach(async ({ page }) => {
  await mockApi(page)
})

test('gateway app loads and protects authenticated routes', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByTestId('login-page')).toBeVisible()

  await page.goto('/dashboard')
  await expect(page).toHaveURL(/\/login\?redirect=\/dashboard/)
})

test('login exposes only platform routes until a signed Contribution is active', async ({ page }) => {
  await page.goto('/login')
  await expect(page.getByTestId('login-page')).toBeVisible()

  await page.getByTestId('login-username').locator('input').fill('operator')
  await page.getByTestId('login-password').locator('input').fill('wrong-password')
  await page.getByTestId('login-submit').click()
  await expect(page.locator('.api-error-alert')).toContainText('Invalid credentials')

  await page.getByTestId('login-password').locator('input').fill('correct-password')
  await page.getByTestId('login-submit').click()
  await expect(page).toHaveURL(/\/dashboard/)

  await page.goto('/problems')
  await expect(page.getByText('Page not found', { exact: true })).toBeVisible()
  await expect(page.getByTestId('problem-list-page')).toHaveCount(0)
})
