import { watch, type WatchStopHandle } from "vue";
import { authenticated, principalId, principalRole } from "../auth";
import { v1Request } from "../api";
import type { Disposable } from "./loader";

const CONTRIBUTION_SNAPSHOT_SCHEMA_V1 = "ojos.dev/contribution-snapshot/v1";
const PERMISSION_CHECK_PATH = "/api/v1/auth/permissions:check";
const MAX_PERMISSION_BATCH = 128;
const PERMISSION_CHECK_TIMEOUT_MS = 5_000;
const digestPattern = /^sha256:[0-9a-f]{64}$/;
const permissionPattern = /^[a-z0-9][a-z0-9.-]*$/;

export interface AdminPermissionQueryV1 {
  current(permission: string): boolean;
  subscribe(listener: () => void): Disposable;
  replaceSnapshot(snapshot: unknown): Promise<void>;
}

export type PermissionCheckTransportV1 = (
  permissions: readonly string[],
  signal: AbortSignal,
) => Promise<unknown>;

export interface RolePermissionQueryOptionsV1 {
  readonly check?: PermissionCheckTransportV1;
}

interface ParsedPermissionSnapshotV1 {
  readonly revision: string;
  readonly permissions: readonly string[];
}

/**
 * Maintains a fail-closed, snapshot-scoped permission cache for contributed
 * admin modules. The browser sends only permission names; the daemon derives
 * the principal exclusively from its HttpOnly session cookie.
 */
export function rolePermissionQuery(
  options: RolePermissionQueryOptionsV1 = {},
): AdminPermissionQueryV1 {
  const check = options.check ?? checkPermissions;
  const listeners = new Set<() => void>();
  let allowed: ReadonlySet<string> = new Set();
  let declared: readonly string[] = Object.freeze([]);
  let snapshotRevision = "";
  let identity = identityRevision();
  let requestGeneration = 0;
  let requestController: AbortController | undefined;
  let stopIdentityWatch: WatchStopHandle | undefined;

  const publish = (next: ReadonlySet<string>) => {
    allowed = next;
    for (const listener of listeners) {
      try {
        listener();
      } catch (error) {
        console.error(
          "[ojos.frontend:admin-shell] permission subscriber failed",
          error,
        );
      }
    }
  };

  const denyImmediately = () => {
    requestController?.abort("permission context superseded");
    requestController = undefined;
    requestGeneration += 1;
    publish(new Set());
  };

  const refresh = async (clearFirst: boolean): Promise<void> => {
    requestController?.abort("permission request superseded");
    if (clearFirst) publish(new Set());
    const generation = ++requestGeneration;
    if (!authenticated.value || declared.length === 0) {
      if (!clearFirst) publish(new Set());
      return;
    }
    const controller = new AbortController();
    requestController = controller;
    try {
      const next = new Set<string>();
      for (let offset = 0; offset < declared.length; offset += MAX_PERMISSION_BATCH) {
        const batch = declared.slice(offset, offset + MAX_PERMISSION_BATCH);
        const raw = await check(batch, controller.signal);
        for (const permission of parseDecisions(raw, batch)) next.add(permission);
      }
      if (generation === requestGeneration && !controller.signal.aborted) {
        publish(next);
      }
    } catch {
      if (generation === requestGeneration && !controller.signal.aborted) {
        publish(new Set());
      }
    } finally {
      if (requestController === controller) requestController = undefined;
    }
  };

  const replaceSnapshot = async (raw: unknown): Promise<void> => {
    let parsed: ParsedPermissionSnapshotV1;
    try {
      parsed = parsePermissionSnapshot(raw);
    } catch (error) {
      declared = Object.freeze([]);
      snapshotRevision = "";
      denyImmediately();
      throw error;
    }
    const changed =
      parsed.revision !== snapshotRevision || !sameStrings(parsed.permissions, declared);
    snapshotRevision = parsed.revision;
    declared = parsed.permissions;
    await refresh(changed);
  };

  const startIdentityWatch = () => {
    if (stopIdentityWatch !== undefined) return;
    identity = identityRevision();
    stopIdentityWatch = watch(
      [authenticated, principalId, principalRole],
      () => {
        const next = identityRevision();
        if (next === identity) return;
        identity = next;
        void refresh(true);
      },
      { flush: "sync" },
    );
  };

  return Object.freeze({
    current(permission: string) {
      return allowed.has(permission);
    },
    subscribe(listener: () => void) {
      listeners.add(listener);
      startIdentityWatch();
      let disposed = false;
      return {
        dispose() {
          if (disposed) return;
          disposed = true;
          listeners.delete(listener);
          if (listeners.size === 0) {
            stopIdentityWatch?.();
            stopIdentityWatch = undefined;
            requestController?.abort("permission query has no subscribers");
            requestController = undefined;
            requestGeneration += 1;
            allowed = new Set();
          }
        },
      };
    },
    replaceSnapshot,
  });
}

async function checkPermissions(
  permissions: readonly string[],
  signal: AbortSignal,
): Promise<unknown> {
  return v1Request(
    "POST",
    PERMISSION_CHECK_PATH,
    { permissions: [...permissions] },
    { signal, timeoutMs: PERMISSION_CHECK_TIMEOUT_MS },
  );
}

function parsePermissionSnapshot(value: unknown): ParsedPermissionSnapshotV1 {
  const snapshot = record(value, "contribution snapshot");
  if (snapshot.schema_version !== CONTRIBUTION_SNAPSHOT_SCHEMA_V1) {
    throw new Error(
      `contribution snapshot schema_version must be ${CONTRIBUTION_SNAPSHOT_SCHEMA_V1}`,
    );
  }
  const revision = text(snapshot.digest, "contribution snapshot digest");
  if (!digestPattern.test(revision)) {
    throw new Error("contribution snapshot digest is invalid");
  }
  if (!Array.isArray(snapshot.permission_definitions)) {
    throw new Error("contribution snapshot permission_definitions must be an array");
  }
  const unique = new Set<string>();
  for (const [index, raw] of snapshot.permission_definitions.entries()) {
    const definition = record(raw, `permission definition ${index}`);
    const permission = text(definition.key, `permission definition ${index} key`);
    if (!validPermission(permission)) {
      throw new Error(`permission definition ${index} key is invalid`);
    }
    if (unique.has(permission)) {
      throw new Error(`permission definition ${permission} is duplicated`);
    }
    unique.add(permission);
  }
  return Object.freeze({
    revision,
    permissions: Object.freeze([...unique].sort()),
  });
}

function parseDecisions(value: unknown, requested: readonly string[]): ReadonlySet<string> {
  const data = exactRecord(value, ["decisions"], "permission check data");
  if (!Array.isArray(data.decisions) || data.decisions.length !== requested.length) {
    throw new Error("permission check decisions do not match the request");
  }
  const expected = new Set(requested);
  const seen = new Set<string>();
  const allowed = new Set<string>();
  for (const [index, raw] of data.decisions.entries()) {
    const decision = exactRecord(raw, ["allowed", "permission"], `permission decision ${index}`);
    const permission = text(decision.permission, `permission decision ${index} permission`);
    if (!expected.has(permission) || seen.has(permission) || typeof decision.allowed !== "boolean") {
      throw new Error(`permission decision ${index} is invalid`);
    }
    seen.add(permission);
    if (decision.allowed) allowed.add(permission);
  }
  if (seen.size !== expected.size) {
    throw new Error("permission check decisions are incomplete");
  }
  return allowed;
}

function identityRevision(): string {
  return JSON.stringify([authenticated.value, principalId.value, principalRole.value]);
}

function validPermission(value: string): boolean {
  return value.length <= 256 && value.includes(".") && permissionPattern.test(value);
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactRecord(
  value: unknown,
  keys: readonly string[],
  label: string,
): Record<string, unknown> {
  const result = record(value, label);
  const actual = Object.keys(result).sort();
  const expected = [...keys].sort();
  if (!sameStrings(actual, expected)) throw new Error(`${label} has unexpected fields`);
  return result;
}

function text(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}
