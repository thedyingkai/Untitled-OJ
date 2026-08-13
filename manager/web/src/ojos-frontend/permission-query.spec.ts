import { beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { authenticated, principalId, principalRole } from "../auth";
import { rolePermissionQuery, type PermissionCheckTransportV1 } from "./permission-query";

const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;

function snapshot(
  permissions = ["contest-service.contest.manage", "contest-service.contest.read"],
  digest = digestA,
) {
  return {
    schema_version: "ojos.dev/contribution-snapshot/v1",
    digest,
    permission_definitions: permissions.map((key) => ({ key })),
  };
}

function decisions(
  permissions: readonly string[],
  allowed = new Set(permissions),
) {
  return {
    decisions: permissions.map((permission) => ({
      permission,
      allowed: allowed.has(permission),
    })),
  };
}

describe("admin Shell contribution permission adapter", () => {
  beforeEach(() => {
    authenticated.value = true;
    principalId.value = "42";
    principalRole.value = "orchestrator.admin";
    delete window.__OJOS_AUTH_READY__;
    delete window.__OJOS_CSRF_TOKEN__;
    vi.restoreAllMocks();
  });

  it("atomically publishes strict allow and deny decisions and notifies subscribers", async () => {
    const check = vi.fn<PermissionCheckTransportV1>().mockImplementation(
      async (permissions) => decisions(permissions, new Set(["contest-service.contest.manage"])),
    );
    const query = rolePermissionQuery({ check });
    const listener = vi.fn();
    const subscription = query.subscribe(listener);

    await query.replaceSnapshot(snapshot());

    expect(check).toHaveBeenCalledWith(
      ["contest-service.contest.manage", "contest-service.contest.read"],
      expect.any(AbortSignal),
    );
    expect(query.current("contest-service.contest.manage")).toBe(true);
    expect(query.current("contest-service.contest.read")).toBe(false);
    expect(query.current("unknown.permission")).toBe(false);
    expect(listener).toHaveBeenCalledTimes(2);
    subscription.dispose();
  });

  it("isolates throwing subscribers and clears grants after the last subscription is disposed", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const check = vi.fn<PermissionCheckTransportV1>().mockImplementation(
      async (permissions) => decisions(permissions),
    );
    const query = rolePermissionQuery({ check });
    const throwing = query.subscribe(() => {
      throw new Error("broken extension listener");
    });
    const healthyListener = vi.fn();
    const healthy = query.subscribe(healthyListener);

    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));

    expect(query.current("contest-service.contest.manage")).toBe(true);
    expect(healthyListener).toHaveBeenCalledTimes(2);
    expect(consoleError).toHaveBeenCalled();
    throwing.dispose();
    healthy.dispose();
    expect(query.current("contest-service.contest.manage")).toBe(false);
  });

  it("uses the same-origin v1 mutation contract without sending a principal", async () => {
    window.__OJOS_CSRF_TOKEN__ = "csrf-memory-only";
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          data: decisions(["contest-service.contest.manage"]),
          meta: { request_id: "req-permissions", api_version: "v1" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const query = rolePermissionQuery();

    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/auth/permissions:check");
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(String(init.body))).toEqual({
      permissions: ["contest-service.contest.manage"],
    });
    expect(String(init.body)).not.toContain("42");
    const headers = init.headers as Record<string, string>;
    expect(headers["x-csrf-token"]).toBe("csrf-memory-only");
    expect(headers["Idempotency-Key"]).toBeTruthy();
    expect(headers.Authorization).toBeUndefined();
  });

  it.each([401, 403])("fails closed when the daemon rejects the session with HTTP %s", async (status) => {
    const granted = new Response(
      JSON.stringify({
        data: decisions(["contest-service.contest.manage"]),
        meta: { request_id: "req-granted", api_version: "v1" },
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
    const rejected = new Response(
      JSON.stringify({
        type: "about:blank",
        title: "Forbidden",
        detail: "permission query rejected",
        code: "PERMISSION_QUERY_REJECTED",
      }),
      {
        status,
        headers: {
          "content-type": "application/problem+json",
          "x-request-id": "req-rejected",
        },
      },
    );
    vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce(granted).mockResolvedValueOnce(rejected));
    const query = rolePermissionQuery();
    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));
    expect(query.current("contest-service.contest.manage")).toBe(true);

    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));

    expect(query.current("contest-service.contest.manage")).toBe(false);
  });

  it.each([
    ["network failure", () => Promise.reject(new Error("offline"))],
    ["missing decision", async () => ({ decisions: [] })],
    [
      "duplicate decision",
      async () => ({
        decisions: [
          { permission: "contest-service.contest.manage", allowed: true },
          { permission: "contest-service.contest.manage", allowed: true },
        ],
      }),
    ],
    [
      "unexpected field",
      async () => ({
        decisions: [
          { permission: "contest-service.contest.manage", allowed: true, principal: "42" },
          { permission: "contest-service.contest.read", allowed: true },
        ],
      }),
    ],
  ])("fails closed on %s", async (_label, implementation) => {
    const check = vi.fn<PermissionCheckTransportV1>().mockImplementation(implementation);
    const query = rolePermissionQuery({ check });

    await query.replaceSnapshot(snapshot());

    expect(query.current("contest-service.contest.manage")).toBe(false);
    expect(query.current("contest-service.contest.read")).toBe(false);
  });

  it("clears stale grants and rechecks when the authenticated identity changes", async () => {
    let allowed = true;
    const check = vi.fn<PermissionCheckTransportV1>().mockImplementation(async (permissions) =>
      decisions(permissions, allowed ? new Set(permissions) : new Set()),
    );
    const query = rolePermissionQuery({ check });
    const listener = vi.fn();
    const subscription = query.subscribe(listener);
    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));
    expect(query.current("contest-service.contest.manage")).toBe(true);

    allowed = false;
    principalId.value = "43";
    expect(query.current("contest-service.contest.manage")).toBe(false);
    await vi.waitFor(() => expect(check).toHaveBeenCalledTimes(2));
    await nextTick();

    expect(query.current("contest-service.contest.manage")).toBe(false);
    expect(listener.mock.calls.length).toBeGreaterThanOrEqual(4);
    subscription.dispose();
  });

  it("clears old grants before replacing a snapshot and ignores superseded results", async () => {
    let resolveOld: ((value: unknown) => void) | undefined;
    const check = vi
      .fn<PermissionCheckTransportV1>()
      .mockResolvedValueOnce(decisions(["contest-service.contest.manage"]))
      .mockImplementationOnce(
        () => new Promise((resolve) => {
          resolveOld = resolve;
        }),
      )
      .mockResolvedValueOnce(decisions(["problem.problem.manage"], new Set()));
    const query = rolePermissionQuery({ check });
    await query.replaceSnapshot(snapshot(["contest-service.contest.manage"]));
    expect(query.current("contest-service.contest.manage")).toBe(true);

    const oldRefresh = query.replaceSnapshot(
      snapshot(["contest-service.contest.read"], digestB),
    );
    expect(query.current("contest-service.contest.manage")).toBe(false);
    const newestRefresh = query.replaceSnapshot(
      snapshot(["problem.problem.manage"], `sha256:${"c".repeat(64)}`),
    );
    await newestRefresh;
    resolveOld?.(decisions(["contest-service.contest.read"]));
    await oldRefresh;

    expect(query.current("contest-service.contest.read")).toBe(false);
    expect(query.current("problem.problem.manage")).toBe(false);
  });

  it("denies malformed snapshots without issuing a permission request", async () => {
    const check = vi.fn<PermissionCheckTransportV1>();
    const query = rolePermissionQuery({ check });

    await expect(
      query.replaceSnapshot({ ...snapshot(), permission_definitions: [{ key: "Bad Permission" }] }),
    ).rejects.toThrow(/key is invalid/);

    expect(check).not.toHaveBeenCalled();
    expect(query.current("contest-service.contest.manage")).toBe(false);
  });
});
