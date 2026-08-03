import { expect, test, type APIRequestContext, type Page } from "@playwright/test";

interface MockSnapshot {
  state: {
    deployments: Array<Record<string, unknown>>;
    nodes: Array<Record<string, unknown>>;
    catalogs: Array<Record<string, unknown>>;
    diagnostics: Array<Record<string, unknown>>;
    revisions: Array<Record<string, unknown>>;
    heads: Record<string, unknown>;
    topologyStatus: Record<string, unknown>;
    layouts: Record<string, { positions?: Record<string, unknown> }>;
    compensations: Array<Record<string, unknown>>;
    capturedMutations: Array<{
      method: string;
      path: string;
      body: Record<string, unknown>;
      idempotency_key: string;
      if_match: string;
    }>;
    contractViolations: string[];
  };
  metrics: {
    abortedRequests: number;
    eventRequests: number;
    layoutPutPaths: string[];
  };
}

async function reset(request: APIRequestContext) {
  const response = await request.post("/__e2e/reset");
  expect(response.ok()).toBeTruthy();
}

async function scenario(
  request: APIRequestContext,
  values: Record<string, unknown>,
) {
  const response = await request.post("/__e2e/scenario", { data: values });
  expect(response.ok()).toBeTruthy();
}

async function snapshot(request: APIRequestContext): Promise<MockSnapshot> {
  const response = await request.get("/__e2e/state");
  expect(response.ok()).toBeTruthy();
  return (await response.json()).data as MockSnapshot;
}

async function openPackage(page: Page) {
  await page.goto("/#/market");
  const card = page.getByTestId("store-package-e2e-api");
  await expect(card).toContainText("E2E API");
  await card.getByRole("button", { name: "安装", exact: true }).click();
  await expect(page.getByRole("heading", { name: "安装 E2E API" })).toBeVisible();
  return card;
}

test.beforeEach(async ({ request }) => {
  await reset(request);
});

test("Store validates a signed release and installs it to a real Running projection", async ({
  page,
  request,
}) => {
  const card = await openPackage(page);

  await page.getByRole("button", { name: "先校验 Release" }).click();
  await expect(page.getByText("Release 校验通过", { exact: true })).toBeVisible();
  await expect(page.getByText(/catalog-e2e-v2/)).toBeVisible();

  await page.getByRole("button", { name: "安装、启动并验证健康" }).click();
  await expect(page.getByText(/安装操作已提交：op-install-/)).toBeVisible();
  await expect(card).toContainText("已部署 1 个");
  await expect(page.getByText(/release\.install reached SUCCEEDED/)).toBeVisible();

  const current = await snapshot(request);
  const deployment = current.state.deployments.find(
    (item) => item.service_id === "e2e-api",
  );
  expect(deployment).toMatchObject({
    desired_state: "RUNNING",
    observed_state: "RUNNING",
    health: "HEALTHY",
  });
  const validate = current.state.capturedMutations.find(
    (item) => item.path === "/api/v1/store/releases:validate",
  );
  expect(validate?.body).toMatchObject({
    service_id: "e2e-api",
    version: "1.2.3",
    catalog_source_id: "trusted-e2e",
    target_node_id: "desktop-local",
  });
  const install = current.state.capturedMutations.find(
    (item) => item.path === "/api/v1/store/releases:install",
  );
  expect(install?.body).toMatchObject({
    service_id: "e2e-api",
    target_node_id: "desktop-local",
    mode: "MANAGED",
    start: true,
    migration_policy: "APPLY",
  });
  expect(install?.idempotency_key).not.toBe("");
  expect(current.state.contractViolations).toEqual([]);
});

test("Catalog management and the complete Store replacement lifecycle remain controllable", async ({
  page,
  request,
}) => {
  await page.goto("/#/market");
  await page.getByRole("button", { name: "管理 Catalog" }).click();
  const catalogModal = page.locator(".modal").filter({ hasText: "受信任 Catalog 来源" });
  await expect(catalogModal.getByText("trusted-e2e", { exact: true })).toBeVisible();
  await catalogModal.getByPlaceholder("production").fill("staging-e2e");
  await catalogModal
    .getByPlaceholder("https://catalog.example/catalog-v2.json")
    .fill("https://catalog.example/staging-v2.json");
  await catalogModal.getByPlaceholder("release-key-2026").fill("staging-key");
  await catalogModal
    .getByPlaceholder("44 字符 padded base64 Ed25519 公钥")
    .fill("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
  await catalogModal.getByRole("button", { name: "注册并验证" }).click();
  const stagingCatalog = catalogModal.locator(".catalog-row").filter({
    hasText: "staging-e2e",
  });
  await expect(stagingCatalog).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  await stagingCatalog.getByRole("button", { name: "移除" }).click();
  await expect(stagingCatalog).toHaveCount(0);
  await catalogModal.getByRole("button", { name: "关闭" }).click();

  const card = page.getByTestId("store-package-e2e-api");
  await card.getByRole("button", { name: "安装", exact: true }).click();
  const installModal = page.locator(".modal").filter({ hasText: "安装 E2E API" });
  await installModal.getByRole("button", { name: "安装、启动并验证健康" }).click();
  await expect(card).toContainText("已部署 1 个");
  await installModal.getByRole("button", { name: "关闭" }).click();

  page.once("dialog", (dialog) => dialog.accept());
  await card.getByRole("button", { name: "升级 desktop-local" }).click();
  await expect
    .poll(async () =>
      (await snapshot(request)).state.deployments.find(
        (deployment) => deployment.deployment_id === "dep-e2e-api",
      )?.version,
    )
    .toBe("1.3.0");

  page.once("dialog", (dialog) => dialog.accept());
  await card.getByRole("button", { name: "回滚 desktop-local" }).click();
  await expect
    .poll(async () =>
      (await snapshot(request)).state.deployments.find(
        (deployment) => deployment.deployment_id === "dep-e2e-api",
      )?.version,
    )
    .toBe("1.2.3");

  page.once("dialog", (dialog) => dialog.accept());
  await card.getByRole("button", { name: "卸载 desktop-local" }).click();
  await expect(card).not.toContainText("已部署 1 个");
  page.once("dialog", (dialog) => dialog.accept());
  await card.getByRole("button", { name: "删除 Release" }).click();
  await expect(card).toHaveCount(0);

  const current = await snapshot(request);
  expect(current.state.catalogs.map((source) => source.id)).toEqual(["trusted-e2e"]);
  const catalogRegistration = current.state.capturedMutations.find(
    (mutation) => mutation.method === "POST" && mutation.path === "/api/v1/store/catalogs",
  );
  expect(catalogRegistration?.body.public_key).toBe(
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  );
  expect(
    current.state.catalogs.some((source) => Object.hasOwn(source, "public_key")),
  ).toBe(false);
  expect(
    current.state.capturedMutations.map((mutation) => mutation.path),
  ).toEqual(
    expect.arrayContaining([
      "/api/v1/store/catalogs",
      "/api/v1/store/catalogs/staging-e2e",
      "/api/v1/store/releases:install",
      "/api/v1/store/releases:upgrade",
      "/api/v1/store/releases:rollback",
      "/api/v1/deployments/dep-e2e-api:uninstall",
      "/api/v1/store/releases:delete",
    ]),
  );
  expect(current.state.contractViolations).toEqual([]);
});

test("Node enrollment, health, drain, certificate revoke and removal are one v1 workflow", async ({
  page,
  request,
}) => {
  await page.goto("/#/nodes");
  await page.getByRole("button", { name: "注册 Node" }).click();
  await page.getByLabel("Node ID").fill("edge-node-02");
  await page.getByLabel("Agent 地址").fill("10.0.0.22");
  await page.getByRole("button", { name: "签发一次性注册码" }).click();
  await expect(page.getByText("one-time-edge-node-02", { exact: true })).toBeVisible();

  let edgeRow = page.getByRole("row").filter({ hasText: "edge-node-01" });
  await edgeRow.getByRole("button", { name: "健康" }).click();
  await expect(page.getByText(/last_heartbeat_at/)).toBeVisible();
  page.on("dialog", (dialog) =>
    dialog.type() === "prompt" ? dialog.accept("scheduled rotation") : dialog.accept(),
  );
  await edgeRow.getByRole("button", { name: "Drain" }).click();
  edgeRow = page.getByRole("row").filter({ hasText: "edge-node-01" });
  await expect(edgeRow.getByText("DRAINED", { exact: true })).toBeVisible();
  await edgeRow.getByRole("button", { name: "吊销证书" }).click();
  await expect(page.getByText(/已吊销 1 张证书/)).toBeVisible();
  await edgeRow.getByRole("button", { name: "移除" }).click();
  await expect(edgeRow).toHaveCount(0);

  const current = await snapshot(request);
  expect(current.state.nodes.some((node) => node.node_id === "edge-node-01")).toBe(false);
  expect(
    current.state.capturedMutations.map((mutation) => mutation.path),
  ).toEqual(
    expect.arrayContaining([
      "/api/v1/nodes/enrollment-codes",
      "/api/v1/nodes/edge-node-01:drain",
      "/api/v1/nodes/edge-node-01:revoke-certificates",
      "/api/v1/nodes/edge-node-01",
    ]),
  );
  expect(current.state.contractViolations).toEqual([]);
});

test("Operation planning and immutable diagnostic create/get/export are reachable", async ({
  page,
  request,
}) => {
  await page.goto("/#/operations");
  await page.getByRole("button", { name: "新建计划" }).click();
  const planModal = page.locator(".modal").filter({ hasText: "新建 Operation 计划" });
  await planModal.getByRole("button", { name: "创建计划" }).click();
  await expect(page.getByText(/计划已创建：op-plan-/)).toBeVisible();
  await expect(page.locator(".modal").filter({ hasText: /操作 op-plan-/ })).toBeVisible();

  await page.goto("/#/diagnostics");
  await expect(page.getByText("diag-1", { exact: true })).toBeVisible();
  const diagnosticRow = page.getByRole("row").filter({ hasText: "diag-1" });
  await diagnosticRow.getByRole("button", { name: "查看" }).click();
  const diagnosticModal = page.locator(".modal").filter({ hasText: "诊断 diag-1" });
  await expect(diagnosticModal.getByText(/topology_id/)).toBeVisible();
  await diagnosticModal.getByRole("button", { name: "关闭" }).click();
  const downloadPromise = page.waitForEvent("download");
  await diagnosticRow.getByRole("button", { name: "导出 JSON" }).click();
  expect((await downloadPromise).suggestedFilename()).toBe("diag-1.json");
  await page.getByRole("button", { name: "创建诊断" }).click();
  await expect(page.getByText("diag-2", { exact: true })).toBeVisible();

  const current = await snapshot(request);
  expect(current.state.diagnostics).toHaveLength(2);
  expect(
    current.state.capturedMutations.map((mutation) => mutation.path),
  ).toEqual(
    expect.arrayContaining(["/api/v1/operations:plan", "/api/v1/diagnostics"]),
  );
  expect(current.state.contractViolations).toEqual([]);
});

test("Store makes failed health compensation explicit and never promotes a Deployment", async ({
  page,
  request,
}) => {
  await scenario(request, { failNextInstall: true });
  await openPackage(page);

  await page.getByRole("button", { name: "安装、启动并验证健康" }).click();
  await expect(page.getByText("安装失败", { exact: true })).toBeVisible();
  await expect(
    page.getByText(/container and Endpoint compensation completed/),
  ).toBeVisible();

  const current = await snapshot(request);
  expect(
    current.state.deployments.some((item) => item.service_id === "e2e-api"),
  ).toBe(false);
  expect(current.state.compensations).toEqual([
    expect.objectContaining({
      service_id: "e2e-api",
      container_removed: true,
      endpoint_removed: true,
      deployment_promoted: false,
    }),
  ]);
});

test("Topology creates an immutable draft revision, validates, diffs, applies and rolls back", async ({
  page,
  request,
}) => {
  await page.goto("/#/topology");
  await expect(page.getByText(/draft r2 · rev-2/)).toBeVisible();
  await expect(page.getByText("DRIFTED · drift 1", { exact: true })).toBeVisible();
  await expect(page.locator('[data-node-id="127.0.0.1:8080:gateway"]')).toBeVisible();

  await page
    .locator('[data-service-id="worker"]')
    .dragTo(page.getByTestId("flow-viewport"), {
      targetPosition: { x: 420, y: 230 },
    });
  const endpointModal = page.locator(".modal").filter({ hasText: "创建端点" });
  await expect(endpointModal).toBeVisible();
  await endpointModal.getByRole("button", { name: "创建端点" }).click();
  await expect(page.getByText(/draft r3 · rev-3/)).toBeVisible();
  await expect(page.locator('[data-node-id="127.0.0.1:8080:worker"]')).toBeVisible();

  await page.getByRole("button", { name: "校验", exact: true }).click();
  await expect(page.getByText(/validated sha256-validated-rev-3/)).toBeVisible();
  await page.getByRole("button", { name: "Diff", exact: true }).click();
  await expect(page.getByText("diff 1 changes", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Apply", exact: true }).click();
  await expect(page.getByText(/operation op-topology-apply-/)).toBeVisible();
  await expect(page.getByText("IN_SYNC · drift 0", { exact: true })).toBeVisible();

  await page.locator(".rollback-control select").selectOption("rev-1");
  await page.getByRole("button", { name: "Rollback", exact: true }).click();
  await expect(page.getByText(/draft r4 · rev-4/)).toBeVisible();
  await expect(page.getByText(/operation op-topology-rollback-/)).toBeVisible();

  const current = await snapshot(request);
  expect(current.state.heads).toMatchObject({
    draft_revision_id: "rev-4",
    applied_revision_id: "rev-4",
  });
  expect(current.state.topologyStatus).toMatchObject({
    desired_revision_id: "rev-4",
    observed_revision_id: "rev-4",
    state: "IN_SYNC",
    drift: [],
  });
  expect(current.state.revisions.at(-1)).toMatchObject({
    revision_id: "rev-4",
    parent_revision_id: "rev-3",
    rollback_of_revision_id: "rev-1",
  });
  expect(current.state.contractViolations).toEqual([]);
});

test("RBAC denial is surfaced without pretending the mutation succeeded", async ({
  page,
  request,
}) => {
  await page.goto("/#/topology");
  await expect(page.getByText(/draft r2 · rev-2/)).toBeVisible();
  await scenario(request, { denyPath: "/api/v1/topologies/primary:validate" });

  await page.getByRole("button", { name: "校验", exact: true }).click();
  await expect(page.getByText(/校验失败：viewer role cannot mutate/)).toBeVisible();
  await expect(page.getByText(/validated /)).toHaveCount(0);

  const current = await snapshot(request);
  expect(
    current.state.capturedMutations.filter(
      (item) => item.path === "/api/v1/topologies/primary:validate",
    ),
  ).toHaveLength(1);
});

test("Operation history shows logs and SSE while retry and cancel remain controllable", async ({
  page,
  request,
}) => {
  await page.goto("/#/operations");

  await page.getByText("op-failed", { exact: true }).click();
  let modal = page.locator(".modal").filter({ hasText: "操作 op-failed" });
  await expect(modal.getByText(/container and endpoint compensation completed/)).toBeVisible();
  await modal.getByRole("button", { name: "重试", exact: true }).click();
  await expect(modal.getByText("SUCCEEDED", { exact: true })).toBeVisible();
  await modal.getByRole("button", { name: "✕" }).click();

  await page.getByText("op-running", { exact: true }).click();
  modal = page.locator(".modal").filter({ hasText: "操作 op-running" });
  await expect(modal.getByText(/restart lease is active/)).toBeVisible();
  const eventCountBeforeCancel = (await snapshot(request)).metrics.eventRequests;
  expect(eventCountBeforeCancel).toBeGreaterThan(0);
  // Let the next long-poll enter the server, then changing the operation out of
  // RUNNING must abort it instead of leaving a zombie request behind.
  await scenario(request, { sseDelayMs: 5_000 });
  await page.waitForTimeout(900);
  await modal.getByRole("button", { name: "取消", exact: true }).click();
  await expect(modal.getByText("CANCELLED", { exact: true })).toBeVisible();
  await expect
    .poll(async () => (await snapshot(request)).metrics.abortedRequests)
    .toBeGreaterThan(0);
  await modal.getByRole("button", { name: "✕" }).click();

  await page.waitForTimeout(1_100);
  const eventCountAfterClose = (await snapshot(request)).metrics.eventRequests;
  await page.waitForTimeout(1_100);
  expect((await snapshot(request)).metrics.eventRequests).toBe(eventCountAfterClose);
  expect(eventCountAfterClose).toBeGreaterThanOrEqual(eventCountBeforeCancel);
});

test("layout persistence is v1, scoped to the current user/topology, and failures stay visible", async ({
  page,
  request,
}) => {
  await scenario(request, { failLayoutSave: true });
  await page.goto("/#/topology");
  await expect(page.getByText(/draft r2 · rev-2/)).toBeVisible();

  await page.getByRole("button", { name: "自动布局", exact: true }).click();
  const persistence = page.getByTestId("layout-persistence-status");
  await expect(persistence).toHaveText("布局未保存");
  await expect(persistence).toHaveAttribute("title", /layout database is full/);

  let current = await snapshot(request);
  expect(current.metrics.layoutPutPaths).toEqual(["/api/v1/ui/layout"]);
  expect(current.state.layouts["e2e-admin:primary"].positions).toEqual({});

  await scenario(request, { failLayoutSave: false });
  await page.getByRole("button", { name: "自动布局", exact: true }).click();
  await expect
    .poll(async () => (await snapshot(request)).metrics.layoutPutPaths.length)
    .toBe(2);
  await expect(persistence).toBeHidden();
  current = await snapshot(request);
  expect(
    Object.keys(current.state.layouts["e2e-admin:primary"].positions || {}),
  ).toEqual([
    "127.0.0.1:8080:gateway",
    "127.0.0.1:5432:database",
  ]);
  expect(current.state.contractViolations).toEqual([]);
});
