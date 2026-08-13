import { expect, test } from "@playwright/test";

import {
  FrontendExtensionLoader,
  parseFrontendManifest,
  satisfiesSemver,
  type FrontendHostServicesV1,
  type FrontendLoaderRuntimeV1,
  type FrontendManifestV1,
  type FrontendModuleV1,
} from "../../src/ojos-frontend/loader";
import {
  adaptContributionSnapshot,
  materializeOperationPath,
  OperationRouteRegistry,
} from "../../src/ojos-frontend/snapshot-adapter";
import {
  FrontendContributionHost,
  FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
  type FrontendDynamicRouteAdapterV1,
  type FrontendExtensionLoaderPortV1,
} from "../../src/ojos-frontend/contribution-host";

const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;
const digestC = `sha256:${"c".repeat(64)}`;

function manifest(overrides: Partial<FrontendManifestV1> = {}): FrontendManifestV1 {
  return {
    schemaVersion: "ojos.frontend/v1",
    moduleId: "contest.user",
    target: "user-shell",
    artifact: "bundle.js",
    hostApiRange: "^1",
    routes: [
      {
        id: "contest",
        path: "/contests",
        title: "Contests",
        menu: true,
        order: 1,
        permission: "contest.read",
      },
    ],
    ...overrides,
  };
}

function host(permissions = new Set(["contest.read"])): FrontendHostServicesV1 {
  const disposable = { dispose: () => undefined };
  const request = async <T = unknown>(): Promise<T> => ({ ok: true }) as T;
  const logger = { debug() {}, info() {}, warn() {}, error() {} };
  return {
    client: { request },
    permissions: { has: (permission) => permissions.has(permission), subscribe: () => disposable },
    theme: { current: () => ({ mode: "dark", variables: {} }), subscribe: () => disposable },
    i18n: { locale: () => "en", translate: (key) => key, subscribe: () => disposable },
    loggerFor: () => logger,
  };
}

function runtime(
  document: Document,
  digests: string[],
  modules: FrontendModuleV1[],
  fetched: string[],
): Partial<FrontendLoaderRuntimeV1> {
  let digestIndex = 0;
  let moduleIndex = 0;
  return {
    origin: "https://shell.example",
    document,
    fetchArtifact: async (url) => {
      fetched.push(url);
      return { ok: true, status: 200, arrayBuffer: async () => new Uint8Array([1]).buffer };
    },
    sha256: async () => digests[Math.min(digestIndex++, digests.length - 1)]!,
    createModuleURL: () => `blob:verified-${moduleIndex}`,
    revokeModuleURL: () => undefined,
    importModule: async () => modules[Math.min(moduleIndex++, modules.length - 1)],
  };
}

class FakeElement {
  hidden = false;
  textContent = "";
  readonly dataset: Record<string, string> = {};
  readonly children: FakeElement[] = [];
  parent?: FakeElement;
  ownerDocument?: Document;

  appendChild(child: FakeElement): FakeElement {
    child.parent = this;
    this.children.push(child);
    return child;
  }

  remove(): void {
    if (this.parent === undefined) return;
    const index = this.parent.children.indexOf(this);
    if (index >= 0) this.parent.children.splice(index, 1);
    this.parent = undefined;
  }

  get firstElementChild(): FakeElement | null {
    return this.children[0] ?? null;
  }
}

function fakeDocument(): Document {
  const document = {
    createElement: () => {
      const element = new FakeElement();
      element.ownerDocument = document as unknown as Document;
      return element;
    },
  };
  return document as unknown as Document;
}

function installRequest(document: Document, bundleDigest = digestA) {
  return {
    manifest: manifest(),
    bundleDigest,
    container: document.createElement("main"),
    surfaceId: "primary",
    routeId: "contest",
  };
}

test("user Shell validates exact ABI and compatible semver", async () => {
  expect(parseFrontendManifest(manifest(), "user-shell").target).toBe("user-shell");
  expect(satisfiesSemver("1.0.0", ">=1.0.0 <2.0.0")).toBe(true);
  expect(satisfiesSemver("1.0.0", "^2")).toBe(false);
  for (const [candidate, code] of [
    [{ ...manifest(), schemaVersion: "ojos.frontend/v2" }, "MANIFEST_INVALID"],
    [{ ...manifest(), target: "admin-shell" }, "TARGET_MISMATCH"],
    [{ ...manifest(), hostApiRange: "^2" }, "HOST_API_INCOMPATIBLE"],
    [{ ...manifest(), artifact: "https://evil.example/code.js" }, "ARTIFACT_INVALID"],
    [{ ...manifest(), token: "forbidden" }, "MANIFEST_INVALID"],
  ] as const) {
    expect(() => parseFrontendManifest(candidate, "user-shell")).toThrow(expect.objectContaining({ code }));
  }
});

test("user Shell verifies content address before import and exposes no token/router/Pinia", async () => {
  const dom = fakeDocument();
  let capturedHost: Record<string, unknown> | undefined;
  const module: FrontendModuleV1 = {
    activate(moduleHost) {
      capturedHost = moduleHost as unknown as Record<string, unknown>;
      return {
        mount(_surface, element) {
          element.textContent = "mounted";
          return { dispose: () => undefined };
        },
        dispose() {},
      };
    },
  };
  const fetched: string[] = [];
  const verifiedRuntime = runtime(dom, [digestA], [module], fetched);
  const importModule = verifiedRuntime.importModule!;
  const importedURLs: string[] = [];
  verifiedRuntime.importModule = async (url) => {
    importedURLs.push(url);
    return importModule(url);
  };
  const loader = new FrontendExtensionLoader({
    target: "user-shell",
    host: host(),
    runtime: verifiedRuntime,
  });
  const request = installRequest(dom);
  await loader.install(request);
  expect(fetched).toEqual([`https://shell.example/__ojos/extensions/${digestA.slice(7)}/bundle.js`]);
  expect(Object.keys(capturedHost!).sort()).toEqual([
    "apiVersion",
    "client",
    "i18n",
    "logger",
    "permissions",
    "theme",
  ]);
  expect(capturedHost).not.toHaveProperty("token");
  expect(capturedHost).not.toHaveProperty("router");
  expect(capturedHost).not.toHaveProperty("pinia");
  expect(importedURLs).toEqual(["blob:verified-0"]);
  expect(request.container.firstElementChild?.textContent).toBe("mounted");
});

test("user Shell gates permission and digest before module import", async () => {
  const document = fakeDocument();
  let imports = 0;
  const module: FrontendModuleV1 = {
    activate: () => ({ mount: () => ({ dispose() {} }), dispose() {} }),
  };
  const fetched: string[] = [];
  const noPermission = new FrontendExtensionLoader({
    target: "user-shell",
    host: host(new Set()),
    runtime: runtime(document, [digestA], [module], fetched),
  });
  await expect(noPermission.install(installRequest(document))).rejects.toMatchObject({ code: "PERMISSION_DENIED" });
  expect(fetched).toHaveLength(0);

  const base = runtime(document, [digestB], [module], fetched);
  const originalImport = base.importModule!;
  base.importModule = async (url) => {
    imports += 1;
    return originalImport(url);
  };
  const tampered = new FrontendExtensionLoader({ target: "user-shell", host: host(), runtime: base });
  await expect(tampered.install(installRequest(document))).rejects.toMatchObject({ code: "DIGEST_MISMATCH" });
  expect(imports).toBe(0);
});

test("user Shell preserves prior module on failed upgrade and disposes it after a valid replacement", async () => {
  const document = fakeDocument();
  const events: string[] = [];
  const prior: FrontendModuleV1 = {
    activate: () => ({
      mount: () => ({ dispose: () => events.push("old-mount-dispose") }),
      dispose: () => events.push("old-activation-dispose"),
    }),
  };
  const broken: FrontendModuleV1 = {
    activate: () => ({ mount: () => { throw new Error("broken"); }, dispose: () => events.push("broken-dispose") }),
  };
  const replacement: FrontendModuleV1 = {
    activate: () => ({
      mount: () => { events.push("new-mount"); return { dispose: () => events.push("new-mount-dispose") }; },
      dispose: () => events.push("new-activation-dispose"),
    }),
  };
  const loader = new FrontendExtensionLoader({
    target: "user-shell",
    host: host(),
    runtime: runtime(document, [digestA, digestB, digestB], [prior, broken, replacement], []),
  });
  const old = await loader.install(installRequest(document, digestA));
  await expect(loader.install(installRequest(document, digestB))).rejects.toMatchObject({ code: "MOUNT_FAILED" });
  expect(loader.active("contest.user")?.bundleDigest).toBe(digestA);
  expect(events).not.toContain("old-mount-dispose");
  const current = await loader.install(installRequest(document, digestB));
  expect(events.slice(-3)).toEqual(["new-mount", "old-mount-dispose", "old-activation-dispose"]);
  await old.dispose();
  expect(loader.active("contest.user")?.bundleDigest).toBe(digestB);
  await current.dispose();
  expect(events).toContain("new-mount-dispose");
  expect(events).toContain("new-activation-dispose");
});

test("user Shell isolates activation timeout", async () => {
  const document = fakeDocument();
  const hung: FrontendModuleV1 = { activate: () => new Promise(() => undefined) };
  const loader = new FrontendExtensionLoader({
    target: "user-shell",
    host: host(),
    timeoutMs: 10,
    runtime: runtime(document, [digestA], [hung], []),
  });
  await expect(loader.install(installRequest(document))).rejects.toMatchObject({ code: "TIMEOUT" });
  expect(loader.active("contest.user")).toBeUndefined();
});

test("user Shell snapshot adapter groups logical module surfaces and filters operation audience", () => {
  const surface = (surfaceId: string, path: string, order: number) => ({
    service_id: "contest-service",
    deployment_id: "contest-deployment",
    revision_id: digestA,
    generation: 1,
    target: "user-shell",
    module_id: "contest.user",
    surface_id: surfaceId,
    route: path,
    menu_label: surfaceId,
    menu: true,
    order,
    permission: "contest.read",
    artifact: "bundle.js",
    host_api_range: "^1",
    manifest_digest: digestB,
    manifest_reference: `https://artifacts.example/__ojos/manifests/${digestB.slice(7)}/manifest.json`,
    bundle_digest: digestC,
    bundle_reference: `https://artifacts.example/__ojos/extensions/${digestC.slice(7)}/bundle.js`,
    enabled: true,
  });
  const raw = {
    schema_version: "ojos.dev/contribution-snapshot/v1",
    digest: digestA,
    user_frontend_modules: [
      surface("contest.detail", "/contests/:id", 2),
      surface("contest.list", "/contests", 1),
    ],
    admin_frontend_modules: [],
    gateway_routes: [
      { enabled: true, audience: "USER", operation_id: "getContest", method: "GET", path: "/api/contests/{contestId}" },
      { enabled: true, audience: "ADMIN", operation_id: "adminList", method: "GET", path: "/api/admin/contests" },
    ],
  };
  const snapshot = adaptContributionSnapshot(raw, "user-shell");
  expect(snapshot.modules).toHaveLength(1);
  expect(snapshot.modules[0]?.manifest.routes.map((route) => route.id)).toEqual([
    "contest.list",
    "contest.detail",
  ]);
  const registry = new OperationRouteRegistry();
  registry.replace(raw, "user-shell");
  expect(materializeOperationPath(registry.resolve("getContest"), {
    params: { contestId: "a/b" },
  })).toBe("/api/contests/a%2Fb");
  expect(() => registry.resolve("adminList")).toThrow(/not ACTIVE/);
});

test("user Shell reuses one activation while switching logical module surfaces", async () => {
  const document = fakeDocument();
  let activations = 0;
  const mounted: string[] = [];
  const logical: FrontendModuleV1 = {
    activate: () => {
      activations += 1;
      return {
        mount: (surfaceId) => {
          mounted.push(surfaceId);
          return { dispose() {} };
        },
        dispose() {},
      };
    },
  };
  const loader = new FrontendExtensionLoader({
    target: "user-shell",
    host: host(),
    runtime: runtime(document, [digestA], [logical], []),
  });
  const multi = manifest({
    routes: [
      { id: "contest", path: "/contests", title: "Contests", menu: true, order: 1, permission: "contest.read" },
      { id: "contest.detail", path: "/contests/:id", title: "Contest", menu: false, order: 2, permission: "contest.read" },
    ],
  });
  await loader.install({ ...installRequest(document), manifest: multi });
  await loader.mountSurface("contest.user", "contest.detail", "contest.detail", { id: "42" });
  expect(activations).toBe(1);
  expect(mounted).toEqual(["primary", "contest.detail"]);
});

test("user Shell host permission-gates definitions and preserves the prior revision on candidate failure", async () => {
  const allowed = new Set<string>();
  let rejectDigest = "";
  const installed: string[] = [];
  const unloaded: string[] = [];
  const loader: FrontendExtensionLoaderPortV1 = {
    async install(request) {
      if (request.bundleDigest === rejectDigest) throw new Error("candidate rejected");
      installed.push(request.bundleDigest);
      const parsed = request.manifest as FrontendManifestV1;
      return { moduleId: parsed.moduleId, bundleDigest: request.bundleDigest, manifest: parsed, dispose() {} };
    },
    async mountSurface() {
      return { moduleId: "contest.user", bundleDigest: digestA, manifest: manifest(), dispose() {} };
    },
    async unload(moduleId) { unloaded.push(moduleId); },
    async dispose() {},
  };
  const registered = new Set<string>();
  const routes: FrontendDynamicRouteAdapterV1 = {
    validate() {},
    register(_moduleId, route) {
      registered.add(route.path);
      return { dispose: () => void registered.delete(route.path) };
    },
  };
  const contribution = (revisionId: string, bundleDigest: string, path: string) => ({
    schemaVersion: FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
    target: "user-shell" as const,
    snapshotRevision: revisionId,
    modules: [{
      revisionId,
      deploymentId: "contest-deployment",
      serviceId: "contest-service",
      generation: revisionId === digestA ? 1 : 2,
      status: "ACTIVE" as const,
      manifest: manifest({ routes: [{
        id: "contest",
        path,
        title: "Contests",
        menu: true,
        order: 1,
        permission: "contest.read",
      }] }),
      manifestDigest: digestB,
      manifestReference: `https://artifacts.example/${digestB.slice(7)}/manifest.json`,
      bundleDigest,
      bundleReference: `https://artifacts.example/${bundleDigest.slice(7)}/bundle.js`,
    }],
  });
  const hostController = new FrontendContributionHost({
    target: "user-shell",
    loader,
    permissions: { has: (permission) => allowed.has(permission), subscribe: () => ({ dispose() {} }) },
    routes,
    fetchSnapshot: async () => contribution(digestA, digestA, "/contests"),
    document: fakeDocument(),
  });

  await hostController.reconcile(contribution(digestA, digestA, "/contests"));
  expect(installed).toEqual([]);
  allowed.add("contest.read");
  await hostController.reconcile(contribution(digestA, digestA, "/contests"));
  expect(registered.has("/contests")).toBe(true);
  rejectDigest = digestC;
  await hostController.reconcile(contribution(digestB, digestC, "/contests-v2"));
  expect(hostController.status().menus.map((item) => item.path)).toEqual(["/contests"]);
  expect(hostController.status().failures[0]?.message).toBe("candidate rejected");
  expect(unloaded).toEqual([]);
  await hostController.dispose();
});
