import { describe, expect, it, vi } from "vitest";

import {
  FrontendExtensionError,
  FrontendExtensionLoader,
  parseFrontendManifest,
  satisfiesSemver,
  type FrontendHostServicesV1,
  type FrontendLoaderRuntimeV1,
  type FrontendManifestV1,
  type FrontendModuleHostV1,
  type FrontendModuleV1,
} from "./loader";

const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;

function manifest(overrides: Partial<FrontendManifestV1> = {}): FrontendManifestV1 {
  return {
    schemaVersion: "ojos.frontend/v1",
    moduleId: "contest.user",
    target: "admin-shell",
    artifact: "bundle.js",
    hostApiRange: "^1.0.0",
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
  const disposable = { dispose: vi.fn() };
  const logger = {
    debug: vi.fn(),
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
  };
  const request = async <T = unknown>(): Promise<T> => ({ ok: true }) as T;
  return {
    client: { request },
    permissions: {
      has: (permission) => permissions.has(permission),
      subscribe: vi.fn(() => disposable),
    },
    theme: {
      current: () => ({ mode: "dark", variables: { accent: "blue" } }),
      subscribe: vi.fn(() => disposable),
    },
    i18n: {
      locale: () => "en",
      translate: (key) => key,
      subscribe: vi.fn(() => disposable),
    },
    loggerFor: vi.fn(() => logger),
  };
}

interface ModuleFixture {
  readonly module: FrontendModuleV1;
  readonly mountDispose: ReturnType<typeof vi.fn>;
  readonly activationDispose: ReturnType<typeof vi.fn>;
  readonly activate: ReturnType<typeof vi.fn>;
  readonly mount: ReturnType<typeof vi.fn>;
}

function moduleFixture(options: { activateError?: Error; mountError?: Error } = {}): ModuleFixture {
  const mountDispose = vi.fn();
  const activationDispose = vi.fn();
  const mount = vi.fn(() => {
    if (options.mountError !== undefined) throw options.mountError;
    return { dispose: mountDispose };
  });
  const activate = vi.fn((moduleHost: FrontendModuleHostV1) => {
    if (options.activateError !== undefined) throw options.activateError;
    return { mount, dispose: activationDispose, moduleHost };
  });
  return { module: { activate }, mountDispose, activationDispose, activate, mount };
}

interface RuntimeFixture {
  readonly runtime: Partial<FrontendLoaderRuntimeV1>;
  readonly fetchedURLs: string[];
  readonly fetchInit: RequestInit[];
  readonly revoked: string[];
  readonly modules: FrontendModuleV1[];
}

function runtimeFixture(digests: string[] = [digestA], modules: FrontendModuleV1[] = [moduleFixture().module]): RuntimeFixture {
  let digestIndex = 0;
  let moduleIndex = 0;
  const fetchedURLs: string[] = [];
  const fetchInit: RequestInit[] = [];
  const revoked: string[] = [];
  return {
    fetchedURLs,
    fetchInit,
    revoked,
    modules,
    runtime: {
      origin: "https://shell.example",
      document,
      fetchArtifact: vi.fn(async (url, init) => {
        fetchedURLs.push(url);
        fetchInit.push(init);
        return { ok: true, status: 200, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer };
      }),
      sha256: vi.fn(async () => digests[Math.min(digestIndex++, digests.length - 1)]!),
      createModuleURL: vi.fn(() => `blob:verified-${moduleIndex}`),
      revokeModuleURL: vi.fn((url) => revoked.push(url)),
      importModule: vi.fn(async () => modules[Math.min(moduleIndex++, modules.length - 1)]),
    },
  };
}

function request(bundleDigest = digestA) {
  return {
    manifest: manifest(),
    bundleDigest,
    container: document.createElement("main"),
    surfaceId: "primary",
    routeId: "contest",
    routeContext: { contestId: "42" },
  };
}

async function expectCode(promise: Promise<unknown>, code: string): Promise<void> {
  await expect(promise).rejects.toMatchObject({ code });
}

describe("ojos.frontend/v1 manifest", () => {
  it("accepts exact schema, target and compatible host range", () => {
    expect(parseFrontendManifest(manifest(), "admin-shell")).toMatchObject({
      schemaVersion: "ojos.frontend/v1",
      moduleId: "contest.user",
      target: "admin-shell",
    });
    expect(satisfiesSemver("1.0.0", "^1")).toBe(true);
    expect(satisfiesSemver("1.0.0", ">=1.0.0 <2.0.0")).toBe(true);
    expect(satisfiesSemver("1.0.0", "~1.0.0")).toBe(true);
    expect(satisfiesSemver("1.0.0", "2.x")).toBe(false);
  });

  it.each([
    [{ ...manifest(), schemaVersion: "ojos.frontend/v2" }, "MANIFEST_INVALID"],
    [{ ...manifest(), target: "user-shell" }, "TARGET_MISMATCH"],
    [{ ...manifest(), hostApiRange: "^2" }, "HOST_API_INCOMPATIBLE"],
    [{ ...manifest(), artifact: "https://evil.example/bundle.js" }, "ARTIFACT_INVALID"],
    [{ ...manifest(), artifact: "../bundle.js" }, "ARTIFACT_INVALID"],
    [{ ...manifest(), token: "secret" }, "MANIFEST_INVALID"],
  ])("rejects invalid manifest %#o", (candidate, code) => {
    expect(() => parseFrontendManifest(candidate, "admin-shell")).toThrowError(
      expect.objectContaining({ code }),
    );
  });
});

describe("FrontendExtensionLoader", () => {
  it("fetches a content-addressed same-origin URL, verifies before import and exposes only the narrow host", async () => {
    const module = moduleFixture();
    const fixture = runtimeFixture([digestA], [module.module]);
    const loader = new FrontendExtensionLoader({
      target: "admin-shell",
      host: host(),
      runtime: fixture.runtime,
    });
    const install = request();
    const active = await loader.install(install);
    expect(active.moduleId).toBe("contest.user");
    expect(fixture.fetchedURLs).toEqual([
      `https://shell.example/__ojos/extensions/${digestA.slice(7)}/bundle.js`,
    ]);
    expect(fixture.fetchInit[0]).toMatchObject({
      credentials: "same-origin",
      redirect: "error",
      cache: "no-store",
    });
    expect(module.activate).toHaveBeenCalledOnce();
    const moduleHost = module.activate.mock.calls[0]![0] as Record<string, unknown>;
    expect(Object.keys(moduleHost).sort()).toEqual([
      "apiVersion",
      "client",
      "i18n",
      "logger",
      "permissions",
      "theme",
    ]);
    expect(moduleHost).not.toHaveProperty("token");
    expect(moduleHost).not.toHaveProperty("router");
    expect(moduleHost).not.toHaveProperty("pinia");
    expect(module.mount.mock.calls[0]![1]).toBe(install.container.firstElementChild);
    expect((install.container.firstElementChild as HTMLElement).hidden).toBe(false);
    expect(fixture.runtime.importModule).toHaveBeenCalledWith("blob:verified-0");
    expect(fixture.revoked).toEqual(["blob:verified-0"]);
  });

  it("permission-gates before artifact fetch", async () => {
    const fixture = runtimeFixture();
    const loader = new FrontendExtensionLoader({
      target: "admin-shell",
      host: host(new Set()),
      runtime: fixture.runtime,
    });
    await expectCode(loader.install(request()), "PERMISSION_DENIED");
    expect(fixture.fetchedURLs).toEqual([]);
  });

  it("rejects tampered bytes before importing", async () => {
    const fixture = runtimeFixture([digestB]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    await expectCode(loader.install(request()), "DIGEST_MISMATCH");
    expect(fixture.runtime.importModule).not.toHaveBeenCalled();
  });

  it("isolates module failures and cleans up a partially activated candidate", async () => {
    const module = moduleFixture({ mountError: new Error("broken mount") });
    const fixture = runtimeFixture([digestA], [module.module]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    const install = request();
    await expectCode(loader.install(install), "MOUNT_FAILED");
    expect(module.activationDispose).toHaveBeenCalledOnce();
    expect(install.container.childElementCount).toBe(0);
    expect(loader.active("contest.user")).toBeUndefined();
  });

  it("preserves the prior module when an upgrade candidate fails", async () => {
    const prior = moduleFixture();
    const broken = moduleFixture({ mountError: new Error("candidate failed") });
    const fixture = runtimeFixture([digestA, digestB], [prior.module, broken.module]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    const first = request(digestA);
    await loader.install(first);
    const priorRoot = first.container.firstElementChild;
    const candidate = request(digestB);
    await expectCode(loader.install(candidate), "MOUNT_FAILED");
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestA);
    expect(prior.mountDispose).not.toHaveBeenCalled();
    expect(prior.activationDispose).not.toHaveBeenCalled();
    expect(first.container.firstElementChild).toBe(priorRoot);
    expect(candidate.container.childElementCount).toBe(0);
  });

  it("mounts an upgrade before disposing the prior revision and makes old handles harmless", async () => {
    const events: string[] = [];
    const prior = moduleFixture();
    prior.mountDispose.mockImplementation(() => events.push("old-mount-dispose"));
    prior.activationDispose.mockImplementation(() => events.push("old-activation-dispose"));
    const candidate = moduleFixture();
    candidate.mount.mockImplementation(() => {
      events.push("new-mount");
      return { dispose: candidate.mountDispose };
    });
    const fixture = runtimeFixture([digestA, digestB], [prior.module, candidate.module]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    const oldHandle = await loader.install(request(digestA));
    const newHandle = await loader.install(request(digestB));
    expect(events).toEqual(["new-mount", "old-mount-dispose", "old-activation-dispose"]);
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestB);
    await oldHandle.dispose();
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestB);
    await newHandle.dispose();
    expect(candidate.mountDispose).toHaveBeenCalledOnce();
    expect(candidate.activationDispose).toHaveBeenCalledOnce();
    expect(loader.active("contest.user")).toBeUndefined();
  });

  it("does not let a throwing scoped logger break activation or rollback", async () => {
    const prior = moduleFixture();
    const broken = moduleFixture({ mountError: new Error("candidate failed") });
    const fixture = runtimeFixture([digestA, digestB], [prior.module, broken.module]);
    const throwingHost = host();
    throwingHost.loggerFor = vi.fn(() => ({
      debug: () => { throw new Error("logger failed"); },
      info: () => { throw new Error("logger failed"); },
      warn: () => { throw new Error("logger failed"); },
      error: () => { throw new Error("logger failed"); },
    }));
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: throwingHost, runtime: fixture.runtime });
    await loader.install(request(digestA));
    await expectCode(loader.install(request(digestB)), "MOUNT_FAILED");
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestA);
    expect(prior.mountDispose).not.toHaveBeenCalled();
  });

  it("times out a hung activation without replacing the active revision", async () => {
    vi.useFakeTimers();
    try {
      const prior = moduleFixture();
      const hung: FrontendModuleV1 = {
        activate: vi.fn(() => new Promise<Awaited<ReturnType<FrontendModuleV1["activate"]>>>(() => undefined)),
      };
      const fixture = runtimeFixture([digestA, digestB], [prior.module, hung]);
      const loader = new FrontendExtensionLoader({
        target: "admin-shell",
        host: host(),
        timeoutMs: 25,
        runtime: fixture.runtime,
      });
      await loader.install(request(digestA));
      const candidate = loader.install(request(digestB));
      const rejection = expectCode(candidate, "TIMEOUT");
      await vi.advanceTimersByTimeAsync(26);
      await rejection;
      expect(loader.active("contest.user")?.bundleDigest).toBe(digestA);
      expect(prior.activationDispose).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("serializes concurrent upgrades for one module", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const first = moduleFixture();
    first.module.activate = vi.fn(async (moduleHost) => {
      await gate;
      return { mount: first.mount, dispose: first.activationDispose, moduleHost };
    });
    const second = moduleFixture();
    const fixture = runtimeFixture([digestA, digestB], [first.module, second.module]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    const firstInstall = loader.install(request(digestA));
    const secondInstall = loader.install(request(digestB));
    for (let attempt = 0; attempt < 10 && fixture.fetchedURLs.length === 0; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    expect(fixture.fetchedURLs).toHaveLength(1);
    release();
    await firstInstall;
    await secondInstall;
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestB);
  });

  it("switches surfaces without importing or activating the logical module again", async () => {
    const fixtureModule = moduleFixture();
    const fixture = runtimeFixture([digestA], [fixtureModule.module]);
    const loader = new FrontendExtensionLoader({
      target: "admin-shell",
      host: host(),
      runtime: fixture.runtime,
    });
    const multiSurface = manifest({
      routes: [
        { id: "contest.list", path: "/admin/contests", title: "List", menu: true, order: 1 },
        { id: "contest.edit", path: "/admin/contests/:id", title: "Edit", menu: false, order: 2 },
      ],
    });
    await loader.install({ ...request(digestA), manifest: multiSurface, routeId: "contest.list" });
    await loader.mountSurface("contest.user", "contest.edit", "contest.edit", { id: "42" });

    expect(fixture.fetchedURLs).toHaveLength(1);
    expect(fixtureModule.module.activate).toHaveBeenCalledOnce();
    expect(fixtureModule.mount).toHaveBeenCalledTimes(2);
    expect(fixtureModule.mountDispose).toHaveBeenCalledOnce();
    expect(fixtureModule.activationDispose).not.toHaveBeenCalled();
  });

  it("preserves the current surface when a candidate surface mount fails", async () => {
    const firstMountDispose = vi.fn();
    const activationDispose = vi.fn();
    const mount = vi.fn(async (surfaceId: string) => {
      if (surfaceId === "contest.edit") throw new Error("edit surface failed");
      return { dispose: firstMountDispose };
    });
    const logicalModule: FrontendModuleV1 = {
      activate: vi.fn(async () => ({ mount, dispose: activationDispose })),
    };
    const fixture = runtimeFixture([digestA], [logicalModule]);
    const loader = new FrontendExtensionLoader({ target: "admin-shell", host: host(), runtime: fixture.runtime });
    const multiSurface = manifest({ routes: [
      { id: "contest.list", path: "/admin/contests", title: "List", menu: true, order: 1 },
      { id: "contest.edit", path: "/admin/contests/:id", title: "Edit", menu: false, order: 2 },
    ] });
    await loader.install({ ...request(digestA), manifest: multiSurface, routeId: "contest.list" });
    await expect(loader.mountSurface("contest.user", "contest.edit", "contest.edit")).rejects.toMatchObject({
      code: "MOUNT_FAILED",
    });
    expect(loader.active("contest.user")?.bundleDigest).toBe(digestA);
    expect(firstMountDispose).not.toHaveBeenCalled();
    expect(activationDispose).not.toHaveBeenCalled();
  });
});

it("FrontendExtensionError carries a stable code", () => {
  const error = new FrontendExtensionError("MODULE_INVALID", "bad module", "contest.user");
  expect(error).toMatchObject({ name: "FrontendExtensionError", code: "MODULE_INVALID", moduleId: "contest.user" });
});
