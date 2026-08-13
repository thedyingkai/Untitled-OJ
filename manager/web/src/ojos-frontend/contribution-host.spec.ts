import { describe, expect, it, vi } from "vitest";
import {
  FrontendContributionHost,
  FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
  type FrontendDynamicRouteAdapterV1,
  type FrontendExtensionLoaderPortV1,
} from "./contribution-host";
import type { ActiveExtensionV1, FrontendManifestV1 } from "./loader";

const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;
const digestC = `sha256:${"c".repeat(64)}`;

function manifest(path = "/admin/contests"): FrontendManifestV1 {
  return {
    schemaVersion: "ojos.frontend/v1",
    moduleId: "contest.admin",
    target: "admin-shell",
    artifact: "bundle.js",
    hostApiRange: "^1",
    routes: [{
      id: "contest.list",
      path,
      title: "Contests",
      menu: true,
      order: 40,
      permission: "contest.manage",
    }],
  };
}

function snapshot(revisionId = digestA, bundleDigest = digestC, path = "/admin/contests") {
  return {
    schemaVersion: FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
    target: "admin-shell",
    snapshotRevision: revisionId,
    modules: [{
      revisionId,
      deploymentId: "contest-deployment",
      serviceId: "contest-service",
      generation: revisionId === digestA ? 1 : 2,
      status: "ACTIVE",
      manifest: manifest(path),
      manifestDigest: digestB,
      manifestReference: `https://artifacts.example/${digestB.slice(7)}/manifest.json`,
      bundleDigest,
      bundleReference: `https://artifacts.example/${bundleDigest.slice(7)}/bundle.js`,
    }],
  };
}

function fakeLoader() {
  let rejectDigest = "";
  const install = vi.fn(async (request): Promise<ActiveExtensionV1> => {
    if (request.bundleDigest === rejectDigest) throw new Error("candidate rejected");
    const typedManifest = request.manifest as FrontendManifestV1;
    return { moduleId: typedManifest.moduleId, bundleDigest: request.bundleDigest, manifest: typedManifest, dispose() {} };
  });
  const mountSurface = vi.fn(async (): Promise<ActiveExtensionV1> => ({
    moduleId: "contest.admin", bundleDigest: digestC, manifest: manifest(), dispose() {},
  }));
  const unload = vi.fn(async () => undefined);
  const dispose = vi.fn(async () => undefined);
  return {
    loader: { install, mountSurface, unload, dispose } as FrontendExtensionLoaderPortV1,
    install,
    mountSurface,
    unload,
    dispose,
    reject(value: string) { rejectDigest = value; },
  };
}

function routeAdapter() {
  const registrations = new Map<string, { dispose: ReturnType<typeof vi.fn> }>();
  const validate = vi.fn();
  const register = vi.fn((_moduleId, route) => {
    const disposable = { dispose: vi.fn(() => void registrations.delete(route.path)) };
    registrations.set(route.path, disposable);
    return disposable;
  });
  return {
    adapter: { validate, register } as FrontendDynamicRouteAdapterV1,
    registrations,
    validate,
    register,
  };
}

describe("FrontendContributionHost", () => {
  it("permission-gates route/menu and registers after candidate activation", async () => {
    const loader = fakeLoader();
    const routes = routeAdapter();
    const allowed = new Set<string>();
    const host = new FrontendContributionHost({
      target: "admin-shell",
      loader: loader.loader,
      permissions: { has: (permission) => allowed.has(permission), subscribe: () => ({ dispose() {} }) },
      routes: routes.adapter,
      fetchSnapshot: async () => snapshot(),
    });
    await host.reconcile(snapshot());
    expect(loader.install).not.toHaveBeenCalled();
    expect(host.status().menus).toEqual([]);

    allowed.add("contest.manage");
    await host.reconcile(snapshot());
    expect(loader.install).toHaveBeenCalledOnce();
    expect(routes.register).toHaveBeenCalledAfter(loader.install);
    expect(host.status().menus).toMatchObject([{ path: "/admin/contests", permission: "contest.manage" }]);
    await host.dispose();
  });

  it("keeps prior revision routes and menus when candidate upgrade fails", async () => {
    const loader = fakeLoader();
    const routes = routeAdapter();
    const host = new FrontendContributionHost({
      target: "admin-shell",
      loader: loader.loader,
      permissions: { has: () => true, subscribe: () => ({ dispose() {} }) },
      routes: routes.adapter,
      fetchSnapshot: async () => snapshot(),
    });
    await host.reconcile(snapshot());
    loader.reject(digestA);
    await host.reconcile(snapshot(digestB, digestA, "/admin/contests-v2"));
    expect(host.status().menus.map((item) => item.path)).toEqual(["/admin/contests"]);
    expect(host.status().failures[0]?.message).toMatch(/candidate rejected/);
    expect(routes.registrations.has("/admin/contests")).toBe(true);
    await host.dispose();
  });

  it("serializes snapshot reconciles and disposes routes/modules on removal", async () => {
    const loader = fakeLoader();
    const routes = routeAdapter();
    const host = new FrontendContributionHost({
      target: "admin-shell",
      loader: loader.loader,
      permissions: { has: () => true, subscribe: () => ({ dispose() {} }) },
      routes: routes.adapter,
      fetchSnapshot: async () => snapshot(),
    });
    await Promise.all([host.reconcile(snapshot()), host.reconcile(snapshot())]);
    expect(loader.install).toHaveBeenCalledOnce();
    await host.reconcile({ ...snapshot(), snapshotRevision: digestB, modules: [] });
    expect(loader.unload).toHaveBeenCalledWith("contest.admin");
    expect(routes.registrations.size).toBe(0);
    await host.dispose();
  });

  it("isolates snapshot fetch failures and keeps the last active revision", async () => {
    const loader = fakeLoader();
    const routes = routeAdapter();
    let unavailable = false;
    const host = new FrontendContributionHost({
      target: "admin-shell",
      loader: loader.loader,
      permissions: { has: () => true, subscribe: () => ({ dispose() {} }) },
      routes: routes.adapter,
      fetchSnapshot: async () => {
        if (unavailable) throw new Error("snapshot offline");
        return snapshot();
      },
    });
    await host.start();
    unavailable = true;
    const current = await host.refresh();
    expect(current.snapshotRevision).toBe(digestA);
    expect(current.menus.map((item) => item.path)).toEqual(["/admin/contests"]);
    expect(current.failures).toMatchObject([{ message: "snapshot offline" }]);
    expect(loader.unload).not.toHaveBeenCalled();
    await host.dispose();
  });
});
