import {
  FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
  type FrontendContributionSnapshotV1,
} from "./contribution-host";
import type {
  ClientCallV1,
  FrontendRouteV1,
  FrontendTarget,
} from "./loader";

export const CONTRIBUTION_SNAPSHOT_SCHEMA_V1 = "ojos.dev/contribution-snapshot/v1";

export interface ContributionSnapshotFetchV1 {
  (signal: AbortSignal): Promise<unknown>;
}

export interface OperationRouteV1 {
  readonly operationId: string;
  readonly method: string;
  readonly path: string;
}

export interface OperationRouteTransportV1 {
  execute<T>(route: OperationRouteV1, call: ClientCallV1): Promise<T>;
}

interface ModuleSurfaceV1 {
  readonly serviceId: string;
  readonly deploymentId: string;
  readonly revisionId: string;
  readonly generation: number;
  readonly moduleId: string;
  readonly surfaceId: string;
  readonly route: FrontendRouteV1;
  readonly artifact: string;
  readonly hostApiRange: string;
  readonly manifestDigest: string;
  readonly manifestReference: string;
  readonly bundleDigest: string;
  readonly bundleReference: string;
}

const digestPattern = /^sha256:[0-9a-f]{64}$/;
const identifierPattern = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}$/;
const methodPattern = /^(DELETE|GET|HEAD|OPTIONS|PATCH|POST|PUT)$/;

export class OperationRouteRegistry {
  private routes = new Map<string, OperationRouteV1>();

  replace(rawSnapshot: unknown, target: FrontendTarget): void {
    const snapshot = record(rawSnapshot, "contribution snapshot");
    const rawRoutes = array(snapshot.gateway_routes, "gateway_routes");
    const audiences = target === "user-shell" ? new Set(["user", "public"]) : new Set(["admin"]);
    const next = new Map<string, OperationRouteV1>();
    for (const [index, value] of rawRoutes.entries()) {
      const item = record(value, `gateway_routes ${index}`);
      const audience = text(item.audience, "audience").toLowerCase();
      if (item.enabled !== true || !audiences.has(audience)) continue;
      const operationId = identifier(item.operation_id, "operation_id");
      const method = text(item.method, "method").toUpperCase();
      if (!methodPattern.test(method)) throw new Error(`gateway route ${operationId} method is invalid`);
      const path = routePath(item.path, `gateway route ${operationId} path`);
      if (next.has(operationId)) throw new Error(`gateway operation ${operationId} is ambiguous`);
      next.set(operationId, Object.freeze({ operationId, method, path }));
    }
    this.routes = next;
  }

  resolve(operationId: string): OperationRouteV1 {
    const route = this.routes.get(operationId);
    if (route === undefined) throw new Error(`operation ${operationId} is not ACTIVE for this Shell`);
    return route;
  }

  clear(): void {
    this.routes.clear();
  }
}

export function createContributionSnapshotFetcher(
  target: FrontendTarget,
  fetchRaw: ContributionSnapshotFetchV1,
  operations: OperationRouteRegistry,
): (requestedTarget: FrontendTarget, signal: AbortSignal) => Promise<unknown> {
  return async (requestedTarget, signal) => {
    if (requestedTarget !== target) throw new Error(`unexpected frontend target ${requestedTarget}`);
    const raw = await fetchRaw(signal);
    const adapted = adaptContributionSnapshot(raw, target);
    operations.replace(raw, target);
    return adapted;
  };
}

export function adaptContributionSnapshot(
  rawSnapshot: unknown,
  target: FrontendTarget,
): FrontendContributionSnapshotV1 {
  const snapshot = record(rawSnapshot, "contribution snapshot");
  if (snapshot.schema_version !== CONTRIBUTION_SNAPSHOT_SCHEMA_V1) {
    throw new Error(`contribution snapshot schema_version must be ${CONTRIBUTION_SNAPSHOT_SCHEMA_V1}`);
  }
  const digest = sha256(snapshot.digest, "contribution snapshot digest");
  const key = target === "user-shell" ? "user_frontend_modules" : "admin_frontend_modules";
  const rows = array(snapshot[key], key).filter((value) => record(value, key).enabled === true);
  const groups = new Map<string, ModuleSurfaceV1[]>();
  for (const [index, value] of rows.entries()) {
    const surface = parseSurface(value, target, `${key} ${index}`);
    const group = groups.get(surface.moduleId) ?? [];
    group.push(surface);
    groups.set(surface.moduleId, group);
  }
  const modules = [...groups.entries()].map(([moduleId, surfaces]) => {
    surfaces.sort((left, right) => left.route.order - right.route.order || left.surfaceId.localeCompare(right.surfaceId));
    const first = surfaces[0]!;
    const routeIDs = new Set<string>();
    const routePaths = new Set<string>();
    for (const surface of surfaces) {
      for (const field of [
        "serviceId",
        "deploymentId",
        "revisionId",
        "generation",
        "artifact",
        "hostApiRange",
        "manifestDigest",
        "manifestReference",
        "bundleDigest",
        "bundleReference",
      ] as const) {
        if (surface[field] !== first[field]) throw new Error(`frontend module ${moduleId} has inconsistent ${field}`);
      }
      if (routeIDs.has(surface.surfaceId) || routePaths.has(surface.route.path)) {
        throw new Error(`frontend module ${moduleId} has a duplicate surface`);
      }
      routeIDs.add(surface.surfaceId);
      routePaths.add(surface.route.path);
    }
    return Object.freeze({
      revisionId: first.revisionId,
      deploymentId: first.deploymentId,
      serviceId: first.serviceId,
      generation: first.generation,
      status: "ACTIVE" as const,
      manifest: Object.freeze({
        schemaVersion: "ojos.frontend/v1" as const,
        moduleId,
        target,
        artifact: first.artifact,
        hostApiRange: first.hostApiRange,
        routes: Object.freeze(surfaces.map((surface) => surface.route)),
      }),
      manifestDigest: first.manifestDigest,
      manifestReference: first.manifestReference,
      bundleDigest: first.bundleDigest,
      bundleReference: first.bundleReference,
    });
  });
  modules.sort((left, right) => left.manifest.moduleId.localeCompare(right.manifest.moduleId));
  return Object.freeze({
    schemaVersion: FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
    target,
    snapshotRevision: digest,
    modules: Object.freeze(modules),
  });
}

export function materializeOperationPath(
  route: OperationRouteV1,
  call: ClientCallV1,
): string {
  const params = call.params ?? {};
  const used = new Set<string>();
  const pathname = route.path.replace(/\{([A-Za-z][A-Za-z0-9_]*)\}/g, (_match, name: string) => {
    const value = params[name];
    if (value === undefined) throw new Error(`operation ${route.operationId} requires path parameter ${name}`);
    used.add(name);
    return encodeURIComponent(String(value));
  });
  const unknown = Object.keys(params).filter((name) => !used.has(name));
  if (unknown.length > 0) throw new Error(`operation ${route.operationId} has unknown path parameter ${unknown[0]}`);
  const query = new URLSearchParams();
  for (const [name, value] of Object.entries(call.query ?? {})) {
    for (const item of Array.isArray(value) ? value : [value]) query.append(name, String(item));
  }
  const suffix = query.toString();
  return suffix === "" ? pathname : `${pathname}?${suffix}`;
}

function parseSurface(value: unknown, target: FrontendTarget, label: string): ModuleSurfaceV1 {
  const item = record(value, label);
  if (item.target !== target) throw new Error(`${label} target must be ${target}`);
  const moduleId = identifier(item.module_id, `${label} module_id`);
  const surfaceId = identifier(item.surface_id, `${label} surface_id`);
  const permission = item.permission == null ? undefined : identifier(item.permission, `${label} permission`);
  const menu = bool(item.menu, `${label} menu`);
  const order = integer(item.order, `${label} order`);
  return Object.freeze({
    serviceId: identifier(item.service_id, `${label} service_id`),
    deploymentId: identifier(item.deployment_id, `${label} deployment_id`),
    revisionId: identifier(item.revision_id, `${label} revision_id`),
    generation: positiveInteger(item.generation, `${label} generation`),
    moduleId,
    surfaceId,
    route: Object.freeze({
      id: surfaceId,
      path: routePath(item.route, `${label} route`),
      title: text(item.menu_label, `${label} menu_label`),
      menu,
      order,
      ...(permission === undefined ? {} : { permission }),
    }),
    artifact: artifactPath(item.artifact, `${label} artifact`),
    hostApiRange: text(item.host_api_range, `${label} host_api_range`),
    manifestDigest: sha256(item.manifest_digest, `${label} manifest_digest`),
    manifestReference: contentAddressedReference(
      item.manifest_reference,
      sha256(item.manifest_digest, `${label} manifest_digest`),
      `${label} manifest_reference`,
    ),
    bundleDigest: sha256(item.bundle_digest, `${label} bundle_digest`),
    bundleReference: contentAddressedReference(
      item.bundle_reference,
      sha256(item.bundle_digest, `${label} bundle_digest`),
      `${label} bundle_reference`,
    ),
  });
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}

function array(value: unknown, label: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  return value;
}

function text(value: unknown, label: string): string {
  if (typeof value !== "string" || value.trim() !== value || value.length < 1 || value.length > 1024) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function identifier(value: unknown, label: string): string {
  const result = text(value, label);
  if (!identifierPattern.test(result)) throw new Error(`${label} is invalid`);
  return result;
}

function sha256(value: unknown, label: string): string {
  const result = text(value, label);
  if (!digestPattern.test(result)) throw new Error(`${label} is invalid`);
  return result;
}

function routePath(value: unknown, label: string): string {
  const result = text(value, label);
  if (!result.startsWith("/") || result.startsWith("//") || result.includes("?") || result.includes("#")) {
    throw new Error(`${label} is invalid`);
  }
  return result;
}

function artifactPath(value: unknown, label: string): string {
  const result = text(value, label);
  if (result.startsWith("/") || result.includes("\\") || result.includes("?") || result.includes("#")) {
    throw new Error(`${label} is invalid`);
  }
  return result;
}

function contentAddressedReference(value: unknown, digest: string, label: string): string {
  const reference = text(value, label);
  let parsed: URL;
  try {
    parsed = new URL(reference);
  } catch {
    throw new Error(`${label} is invalid`);
  }
  if (parsed.protocol !== "https:" || parsed.username !== "" || parsed.password !== "") {
    throw new Error(`${label} must be HTTPS`);
  }
  if (!parsed.pathname.includes(digest.slice("sha256:".length))) {
    throw new Error(`${label} is not content-addressed by its digest`);
  }
  return reference;
}

function bool(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new Error(`${label} is invalid`);
  return value;
}

function integer(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < -1_000_000 || (value as number) > 1_000_000) {
    throw new Error(`${label} is invalid`);
  }
  return value as number;
}

function positiveInteger(value: unknown, label: string): number {
  const result = integer(value, label);
  if (result < 1) throw new Error(`${label} is invalid`);
  return result;
}
