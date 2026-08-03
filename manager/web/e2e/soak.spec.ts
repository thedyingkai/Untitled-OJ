import { expect, test } from "@playwright/test";

const durationMs = Math.max(1_000, Number(process.env.OJOS_E2E_SOAK_MS || 5_000));
const expectedOrigin = `http://127.0.0.1:${process.env.OJOS_E2E_PORT || "4174"}`;

test("Web remains responsive with bounded polling for the configured soak window", async ({
  page,
  request,
}) => {
  test.setTimeout(durationMs + 30_000);
  await request.post("/__e2e/reset");
  const pageErrors: string[] = [];
  const popups: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") pageErrors.push(message.text());
  });
  page.on("popup", (popup) => popups.push(popup.url()));
  // Exercise the exact bootstrap branch used by the Tauri WebView. The secret
  // exchange itself is owned by Desktop; the bundle only receives its bounded
  // readiness promise and same-origin HttpOnly session.
  await page.addInitScript(() => {
    (window as Window & { __OJOS_AUTH_READY__?: Promise<void> }).__OJOS_AUTH_READY__ =
      Promise.resolve();
  });

  await page.goto("/#/topology");
  await expect(page.getByText(/draft r2 · rev-2/)).toBeVisible();
  await expect(page.getByText("Desktop 本地会话", { exact: true })).toBeVisible();
  const deadline = Date.now() + durationMs;
  let iterations = 0;
  while (Date.now() < deadline) {
    await expect(page.locator(".shell")).toBeVisible();
    await page.evaluate(
      () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())),
    );
    iterations += 1;
    await page.waitForTimeout(Math.min(500, Math.max(1, deadline - Date.now())));
  }

  await page.getByRole("link", { name: "商店" }).click();
  await expect(page.getByTestId("store-package-e2e-api")).toBeVisible();
  await page.getByRole("link", { name: "拓扑" }).click();
  await expect(page.getByText(/draft r2 · rev-2/)).toBeVisible();
  expect(iterations).toBeGreaterThan(0);
  expect(pageErrors).toEqual([]);
  expect(popups).toEqual([]);
  expect(new URL(page.url()).origin).toBe(expectedOrigin);

  const response = await request.get("/__e2e/metrics");
  const metrics = (await response.json()).data as {
    activeRequests: number;
    maxConcurrentRequests: number;
    corePollRequests: number;
    paths: Record<string, number>;
  };
  expect(metrics.activeRequests).toBe(0);
  // One refresh consists of six parallel bounded reads, plus layout/store reads.
  expect(metrics.maxConcurrentRequests).toBeLessThanOrEqual(10);
  const maximumCoreReads = (Math.ceil(durationMs / 3_500) + 3) * 6;
  expect(metrics.corePollRequests).toBeLessThanOrEqual(maximumCoreReads);
  expect(metrics.paths["/api/v1/auth/config"] || 0).toBe(0);
});
