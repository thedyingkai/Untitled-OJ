import { describe, expect, it } from "vitest";
import {
  adaptContributionSnapshot,
  createContributionSnapshotFetcher,
  materializeOperationPath,
  OperationRouteRegistry,
} from "./snapshot-adapter";

const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;
const digestC = `sha256:${"c".repeat(64)}`;

function surface(overrides: Record<string, unknown> = {}) {
  return {
    service_id: "contest-service",
    deployment_id: "contest-deployment",
    revision_id: digestA,
    generation: 2,
    target: "admin-shell",
    module_id: "contest.admin",
    surface_id: "contest.list",
    route: "/admin/contests",
    menu_label: "Contests",
    menu: true,
    order: 40,
    permission: "contest.manage",
    artifact: "bundle.js",
    host_api_range: "^1",
    manifest_digest: digestB,
    manifest_reference: `https://artifacts.example/__ojos/manifests/${digestB.slice(7)}/manifest.json`,
    bundle_digest: digestC,
    bundle_reference: `https://artifacts.example/__ojos/extensions/${digestC.slice(7)}/bundle.js`,
    enabled: true,
    ...overrides,
  };
}

function snapshot(rows: unknown[] = [surface()]) {
  return {
    schema_version: "ojos.dev/contribution-snapshot/v1",
    digest: digestA,
    user_frontend_modules: [],
    admin_frontend_modules: rows,
    gateway_routes: [],
  };
}

describe("Contribution snapshot adapter", () => {
  it("groups multiple surfaces into one logical module manifest", () => {
    const adapted = adaptContributionSnapshot(
      snapshot([
        surface({ surface_id: "contest.edit", route: "/admin/contests/:id", menu: false, order: 50 }),
        surface(),
      ]),
      "admin-shell",
    );

    expect(adapted.snapshotRevision).toBe(digestA);
    expect(adapted.modules).toHaveLength(1);
    expect(adapted.modules[0]).toMatchObject({
      status: "ACTIVE",
      manifestDigest: digestB,
      manifestReference: `https://artifacts.example/__ojos/manifests/${digestB.slice(7)}/manifest.json`,
      bundleDigest: digestC,
      bundleReference: `https://artifacts.example/__ojos/extensions/${digestC.slice(7)}/bundle.js`,
      manifest: { moduleId: "contest.admin", target: "admin-shell", artifact: "bundle.js" },
    });
    expect(adapted.modules[0]?.manifest.routes.map((route) => route.id)).toEqual([
      "contest.list",
      "contest.edit",
    ]);
  });

  it("ignores non-ready modules and rejects inconsistent module artifacts", () => {
    expect(adaptContributionSnapshot(snapshot([surface({ enabled: false })]), "admin-shell").modules).toEqual([]);
    expect(() => adaptContributionSnapshot(snapshot([
      surface(),
      surface({ surface_id: "contest.edit", route: "/admin/contests/:id", artifact: "other.js" }),
    ]), "admin-shell")).toThrow(/inconsistent artifact/);
  });

  it("rejects target mismatch and missing ABI fields instead of guessing defaults", () => {
    expect(() => adaptContributionSnapshot(snapshot([surface({ target: "user-shell" })]), "admin-shell"))
      .toThrow(/target must be admin-shell/);
    expect(() => adaptContributionSnapshot(snapshot([surface({ host_api_range: undefined })]), "admin-shell"))
      .toThrow(/host_api_range is invalid/);
    expect(() => adaptContributionSnapshot(snapshot([surface({
      bundle_reference: "https://artifacts.example/bundles/not-content-addressed.js",
    })]), "admin-shell")).toThrow(/not content-addressed/);
  });

  it("publishes a snapshot observer only after modules and routes validate", async () => {
    const operations = new OperationRouteRegistry();
    const observed: unknown[] = [];
    const valid = snapshot();
    const fetcher = createContributionSnapshotFetcher(
      "admin-shell",
      async () => valid,
      operations,
      (raw) => observed.push(raw),
    );

    await expect(fetcher("admin-shell", new AbortController().signal)).resolves.toMatchObject({
      snapshotRevision: digestA,
    });
    expect(observed).toEqual([valid]);

    const invalidFetcher = createContributionSnapshotFetcher(
      "admin-shell",
      async () => snapshot([surface({ target: "user-shell" })]),
      operations,
      (raw) => observed.push(raw),
    );
    await expect(
      invalidFetcher("admin-shell", new AbortController().signal),
    ).rejects.toThrow(/target must be admin-shell/);
    expect(observed).toEqual([valid]);
  });
});

describe("Operation route registry", () => {
  it("publishes only enabled routes for the Shell audience", () => {
    const registry = new OperationRouteRegistry();
    registry.replace({
      ...snapshot([]),
      gateway_routes: [
        { enabled: true, audience: "ADMIN", operation_id: "adminList", method: "GET", path: "/api/admin/contests" },
        { enabled: true, audience: "USER", operation_id: "userList", method: "GET", path: "/api/contests" },
      ],
    }, "admin-shell");
    expect(registry.resolve("adminList").path).toBe("/api/admin/contests");
    expect(() => registry.resolve("userList")).toThrow(/not ACTIVE/);
  });

  it("materializes encoded path parameters and query without accepting extras", () => {
    const route = { operationId: "getContest", method: "GET", path: "/api/contests/{contestId}" };
    expect(materializeOperationPath(route, {
      params: { contestId: "a/b" },
      query: { include: ["owner", "entries"], page: 2 },
    })).toBe("/api/contests/a%2Fb?include=owner&include=entries&page=2");
    expect(() => materializeOperationPath(route, { params: { wrong: 1 } })).toThrow(/requires path parameter/);
  });
});
