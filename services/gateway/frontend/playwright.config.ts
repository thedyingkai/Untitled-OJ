import { defineConfig, devices } from '@playwright/test'

const localNoProxy = ['127.0.0.1', 'localhost', '::1']
process.env.NO_PROXY = [process.env.NO_PROXY, ...localNoProxy].filter(Boolean).join(',')
process.env.no_proxy = [process.env.no_proxy, ...localNoProxy].filter(Boolean).join(',')

export default defineConfig({
  testDir: './tests/e2e',
  timeout: 30_000,
  expect: {
    timeout: 10_000,
  },
  fullyParallel: true,
  reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:4173',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  webServer: {
    command: 'npm run build && npm run preview:e2e',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
})
