import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";

const port = Number(process.env.OJOS_E2E_PORT || 4174);
const baseURL = `http://127.0.0.1:${port}`;
const windowsEdge = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const localExecutable =
  process.platform === "win32" && existsSync(windowsEdge) ? windowsEdge : undefined;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 7_500 },
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [["line"], ["html", { open: "never", outputFolder: "playwright-report" }]]
    : "list",
  use: {
    ...devices["Desktop Chrome"],
    baseURL,
    headless: true,
    launchOptions: localExecutable ? { executablePath: localExecutable } : undefined,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
  },
  webServer: {
    command: "node e2e/mock-control-plane.mjs",
    url: `${baseURL}/api/v1/healthz/ready`,
    timeout: 15_000,
    reuseExistingServer: !process.env.CI,
    env: { OJOS_E2E_PORT: String(port) },
  },
});
