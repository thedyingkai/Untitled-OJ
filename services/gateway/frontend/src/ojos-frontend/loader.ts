export const FRONTEND_SCHEMA_VERSION = "ojos.frontend/v1";
export const FRONTEND_HOST_API_VERSION = "1.0.0";

export type FrontendTarget = "user-shell" | "admin-shell";

export interface FrontendRouteV1 {
  readonly id: string;
  readonly path: string;
  readonly title: string;
  readonly menu: boolean;
  readonly order: number;
  readonly permission?: string;
}

export interface FrontendManifestV1 {
  readonly schemaVersion: typeof FRONTEND_SCHEMA_VERSION;
  readonly moduleId: string;
  readonly target: FrontendTarget;
  readonly artifact: string;
  readonly hostApiRange: string;
  readonly routes: readonly FrontendRouteV1[];
}

export interface Disposable {
  dispose(): void | Promise<void>;
}

export interface ClientCallV1 {
  readonly params?: Readonly<Record<string, string | number | boolean>>;
  readonly query?: Readonly<Record<string, string | number | boolean | readonly string[]>>;
  readonly body?: unknown;
  readonly signal?: AbortSignal;
  readonly idempotencyKey?: string;
}

export interface AuthenticatedClientTransportV1 {
  request<T = unknown>(operationId: string, call?: ClientCallV1): Promise<T>;
}

export interface PermissionHostServiceV1 {
  has(permission: string): boolean;
  subscribe(listener: () => void): Disposable;
}

export interface ThemeSnapshotV1 {
  readonly mode: string;
  readonly variables: Readonly<Record<string, string>>;
}

export interface ThemeHostServiceV1 {
  current(): ThemeSnapshotV1;
  subscribe(listener: (theme: ThemeSnapshotV1) => void): Disposable;
}

export interface I18nHostServiceV1 {
  locale(): string;
  translate(key: string, values?: Readonly<Record<string, string | number>>): string;
  subscribe(listener: (locale: string) => void): Disposable;
}

export interface ScopedLoggerV1 {
  debug(message: string, fields?: Readonly<Record<string, unknown>>): void;
  info(message: string, fields?: Readonly<Record<string, unknown>>): void;
  warn(message: string, fields?: Readonly<Record<string, unknown>>): void;
  error(message: string, fields?: Readonly<Record<string, unknown>>): void;
}

export interface FrontendHostServicesV1 {
  readonly client: AuthenticatedClientTransportV1;
  readonly permissions: PermissionHostServiceV1;
  readonly theme: ThemeHostServiceV1;
  readonly i18n: I18nHostServiceV1;
  loggerFor(moduleId: string): ScopedLoggerV1;
}

// This is the complete module-facing host surface. Authentication material,
// Router, Pinia and other Shell internals are deliberately absent.
export interface FrontendModuleHostV1 {
  readonly apiVersion: typeof FRONTEND_HOST_API_VERSION;
  readonly client: AuthenticatedClientTransportV1;
  readonly permissions: PermissionHostServiceV1;
  readonly theme: ThemeHostServiceV1;
  readonly i18n: I18nHostServiceV1;
  readonly logger: ScopedLoggerV1;
}

export interface FrontendActivationV1 {
  mount(
    surfaceId: string,
    element: HTMLElement,
    routeContext: Readonly<Record<string, unknown>>,
  ): Disposable | Promise<Disposable>;
  dispose(): void | Promise<void>;
}

export interface FrontendModuleV1 {
  activate(host: FrontendModuleHostV1): FrontendActivationV1 | Promise<FrontendActivationV1>;
}

export interface ArtifactResponseV1 {
  readonly ok: boolean;
  readonly status: number;
  arrayBuffer(): Promise<ArrayBuffer>;
}

export interface FrontendLoaderRuntimeV1 {
  readonly origin?: string;
  readonly document?: Document;
  fetchArtifact(url: string, init: RequestInit): Promise<ArtifactResponseV1>;
  sha256(bytes: ArrayBuffer): Promise<string>;
  createModuleURL(blob: Blob): string;
  revokeModuleURL(url: string): void;
  importModule(url: string): Promise<unknown>;
}

export interface FrontendLoaderOptionsV1 {
  readonly target: FrontendTarget;
  readonly host: FrontendHostServicesV1;
  readonly timeoutMs?: number;
  readonly runtime?: Partial<FrontendLoaderRuntimeV1>;
}

export interface InstallExtensionRequestV1 {
  readonly manifest: unknown;
  readonly bundleDigest: string;
  readonly container: HTMLElement;
  readonly surfaceId: string;
  readonly routeId?: string;
  readonly routeContext?: Readonly<Record<string, unknown>>;
  readonly timeoutMs?: number;
}

export interface ActiveExtensionV1 extends Disposable {
  readonly moduleId: string;
  readonly bundleDigest: string;
  readonly manifest: FrontendManifestV1;
}

export type FrontendExtensionErrorCode =
  | "MANIFEST_INVALID"
  | "TARGET_MISMATCH"
  | "HOST_API_INCOMPATIBLE"
  | "PERMISSION_DENIED"
  | "ARTIFACT_INVALID"
  | "ARTIFACT_FETCH_FAILED"
  | "DIGEST_MISMATCH"
  | "MODULE_INVALID"
  | "ACTIVATE_FAILED"
  | "MOUNT_FAILED"
  | "TIMEOUT";

export class FrontendExtensionError extends Error {
  readonly code: FrontendExtensionErrorCode;
  readonly moduleId: string;
  readonly cause?: unknown;

  constructor(
    code: FrontendExtensionErrorCode,
    message: string,
    moduleId = "",
    cause?: unknown,
  ) {
    super(message);
    this.name = "FrontendExtensionError";
    this.code = code;
    this.moduleId = moduleId;
    this.cause = cause;
  }
}

interface ActiveRecordV1 {
  readonly manifest: FrontendManifestV1;
  readonly bundleDigest: string;
  root: HTMLElement;
  readonly activation: FrontendActivationV1;
  mountDisposable: Disposable;
  readonly logger: ScopedLoggerV1;
  readonly timeoutMs: number;
  disposed: boolean;
}

interface ParsedVersion {
  readonly major: number;
  readonly minor: number;
  readonly patch: number;
}

interface PartialVersion {
  readonly values: readonly number[];
  readonly wildcard: boolean;
}

const identifierPattern = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}$/;
const permissionPattern = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}$/;
const digestPattern = /^sha256:[0-9a-f]{64}$/;
const artifactSegmentPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,255}$/;
const allowedManifestKeys = [
  "schemaVersion",
  "moduleId",
  "target",
  "artifact",
  "hostApiRange",
  "routes",
] as const;
const allowedRouteKeys = ["id", "path", "title", "menu", "order", "permission"] as const;

export class FrontendExtensionLoader {
  private readonly target: FrontendTarget;
  private readonly host: FrontendHostServicesV1;
  private readonly timeoutMs: number;
  private readonly runtime: FrontendLoaderRuntimeV1;
  private readonly activeRecords = new Map<string, ActiveRecordV1>();
  private readonly operations = new Map<string, Promise<void>>();

  constructor(options: FrontendLoaderOptionsV1) {
    if (options.target !== "user-shell" && options.target !== "admin-shell") {
      throw new FrontendExtensionError("TARGET_MISMATCH", "Shell target is invalid");
    }
    this.target = options.target;
    this.host = validateHostServices(options.host);
    this.timeoutMs = validateTimeout(options.timeoutMs ?? 10_000);
    this.runtime = createRuntime(options.runtime);
  }

  install(request: InstallExtensionRequestV1): Promise<ActiveExtensionV1> {
    let manifest: FrontendManifestV1;
    try {
      manifest = parseFrontendManifest(request.manifest, this.target);
    } catch (error) {
      return Promise.reject(error);
    }
    return this.serialize(manifest.moduleId, () => this.installSerialized(manifest, request));
  }

  mountSurface(
    moduleId: string,
    surfaceId: string,
    routeId: string,
    routeContext: Readonly<Record<string, unknown>> = {},
  ): Promise<ActiveExtensionV1> {
    return this.serialize(moduleId, async () => {
      const record = this.activeRecords.get(moduleId);
      if (record === undefined) {
        throw new FrontendExtensionError(
          "MOUNT_FAILED",
          `Frontend module ${moduleId} is not active`,
          moduleId,
        );
      }
      const route = selectRoute(record.manifest, routeId);
      if (route?.permission !== undefined && !this.host.permissions.has(route.permission)) {
        throw new FrontendExtensionError(
          "PERMISSION_DENIED",
          `Permission ${route.permission} is required by frontend module ${moduleId}`,
          moduleId,
        );
      }
      if (!identifierPattern.test(surfaceId)) {
        throw new FrontendExtensionError(
          "MOUNT_FAILED",
          "Frontend extension surfaceId is invalid",
          moduleId,
        );
      }
      const candidateRoot = this.runtime.document!.createElement("div");
      candidateRoot.hidden = true;
      candidateRoot.dataset.ojosFrontendModule = moduleId;
      record.root.parentElement?.appendChild(candidateRoot);
      let candidateMount: Disposable | undefined;
      try {
        candidateMount = await withinTimeout(
          Promise.resolve().then(() =>
            record.activation.mount(
              surfaceId,
              candidateRoot,
              Object.freeze({ ...routeContext }),
            ),
          ),
          record.timeoutMs,
          "MOUNT_FAILED",
          `Frontend module ${moduleId} mount timed out`,
          moduleId,
          undefined,
          (lateDisposable) =>
            disposeOne(lateDisposable, record.logger, "late mount", record.timeoutMs),
        );
        candidateMount = validateDisposable(candidateMount, moduleId, "mount");
        const previousMount = record.mountDisposable;
        const previousRoot = record.root;
        record.mountDisposable = candidateMount;
        record.root = candidateRoot;
        await disposeOne(previousMount, record.logger, "previous mount", record.timeoutMs);
        previousRoot.remove();
        candidateRoot.hidden = false;
        return this.publicHandle(record);
      } catch (cause) {
        if (candidateMount !== undefined) {
          await disposeOne(candidateMount, record.logger, "candidate mount", record.timeoutMs);
        }
        candidateRoot.remove();
        throw normalizeLoadError(cause, moduleId);
      }
    });
  }

  active(moduleId: string): ActiveExtensionV1 | undefined {
    const record = this.activeRecords.get(moduleId);
    return record === undefined ? undefined : this.publicHandle(record);
  }

  async unload(moduleId: string): Promise<void> {
    await this.serialize(moduleId, async () => {
      const record = this.activeRecords.get(moduleId);
      if (record === undefined) return;
      this.activeRecords.delete(moduleId);
      await this.disposeRecord(record);
    });
  }

  async dispose(): Promise<void> {
    const moduleIds = [...this.activeRecords.keys()];
    await Promise.all(moduleIds.map((moduleId) => this.unload(moduleId)));
  }

  private async installSerialized(
    manifest: FrontendManifestV1,
    request: InstallExtensionRequestV1,
  ): Promise<ActiveExtensionV1> {
    const logger = safeLogger(this.host.loggerFor(manifest.moduleId));
    const timeoutMs = validateTimeout(request.timeoutMs ?? this.timeoutMs);
    const route = selectRoute(manifest, request.routeId);
    if (route?.permission !== undefined && !this.host.permissions.has(route.permission)) {
      const error = new FrontendExtensionError(
        "PERMISSION_DENIED",
        `Permission ${route.permission} is required by frontend module ${manifest.moduleId}`,
        manifest.moduleId,
      );
      logger.warn("frontend extension permission denied", {
        moduleId: manifest.moduleId,
        permission: route.permission,
      });
      throw error;
    }
    if (!isElementLike(request.container)) {
      throw new FrontendExtensionError(
        "MOUNT_FAILED",
        "Frontend extension container is invalid",
        manifest.moduleId,
      );
    }
    if (!identifierPattern.test(request.surfaceId)) {
      throw new FrontendExtensionError(
        "MOUNT_FAILED",
        "Frontend extension surfaceId is invalid",
        manifest.moduleId,
      );
    }
    const bundleDigest = canonicalDigest(request.bundleDigest, manifest.moduleId);
    const artifactURL = buildArtifactURL(this.runtime.origin ?? "", bundleDigest, manifest.artifact);
    const moduleHost = createModuleHost(this.host, manifest.moduleId);
    let activation: FrontendActivationV1 | undefined;
    let mountDisposable: Disposable | undefined;
    let root: HTMLElement | undefined;
    try {
      const namespace = await this.loadVerifiedModule(
        artifactURL,
        bundleDigest,
        manifest.moduleId,
        timeoutMs,
      );
      const module = validateModule(namespace, manifest.moduleId);
      activation = await withinTimeout(
        Promise.resolve().then(() => module.activate(moduleHost)),
        timeoutMs,
        "ACTIVATE_FAILED",
        `Frontend module ${manifest.moduleId} activation timed out`,
        manifest.moduleId,
        undefined,
        (lateActivation) => disposeActivation(lateActivation, logger, timeoutMs),
      );
      activation = validateActivation(activation, manifest.moduleId);
      root = this.runtime.document!.createElement("div");
      root.hidden = true;
      root.dataset.ojosFrontendModule = manifest.moduleId;
      request.container.appendChild(root);
      const routeContext = Object.freeze({ ...(request.routeContext ?? {}) });
      mountDisposable = await withinTimeout(
        Promise.resolve().then(() => activation!.mount(request.surfaceId, root!, routeContext)),
        timeoutMs,
        "MOUNT_FAILED",
        `Frontend module ${manifest.moduleId} mount timed out`,
        manifest.moduleId,
        undefined,
        (lateDisposable) => disposeOne(lateDisposable, logger, "late mount", timeoutMs),
      );
      mountDisposable = validateDisposable(mountDisposable, manifest.moduleId, "mount");
      const candidate: ActiveRecordV1 = {
        manifest,
        bundleDigest,
        root,
        activation,
        mountDisposable,
        logger,
        timeoutMs,
        disposed: false,
      };
      const previous = this.activeRecords.get(manifest.moduleId);
      if (previous !== undefined) {
        await this.disposeRecord(previous);
      }
      root.hidden = false;
      this.activeRecords.set(manifest.moduleId, candidate);
      logger.info("frontend extension activated", {
        moduleId: manifest.moduleId,
        bundleDigest,
        target: manifest.target,
      });
      return this.publicHandle(candidate);
    } catch (cause) {
      if (mountDisposable !== undefined) {
        await disposeOne(mountDisposable, logger, "candidate mount", timeoutMs);
      }
      if (activation !== undefined) {
        await disposeActivation(activation, logger, timeoutMs);
      }
      root?.remove();
      const error = normalizeLoadError(cause, manifest.moduleId);
      logger.error("frontend extension candidate rejected", {
        moduleId: manifest.moduleId,
        code: error.code,
        message: error.message,
      });
      throw error;
    }
  }

  private async loadVerifiedModule(
    artifactURL: string,
    expectedDigest: string,
    moduleId: string,
    timeoutMs: number,
  ): Promise<unknown> {
    const controller = new AbortController();
    const response = await withinTimeout(
      this.runtime.fetchArtifact(artifactURL, {
        method: "GET",
        credentials: "same-origin",
        redirect: "error",
        cache: "no-store",
        signal: controller.signal,
        headers: { Accept: "text/javascript, application/javascript" },
      }),
      timeoutMs,
      "ARTIFACT_FETCH_FAILED",
      `Frontend module ${moduleId} fetch timed out`,
      moduleId,
      () => controller.abort(),
    );
    if (!response.ok) {
      throw new FrontendExtensionError(
        "ARTIFACT_FETCH_FAILED",
        `Frontend module ${moduleId} artifact returned HTTP ${response.status}`,
        moduleId,
      );
    }
    const bytes = await withinTimeout(
      response.arrayBuffer(),
      timeoutMs,
      "ARTIFACT_FETCH_FAILED",
      `Frontend module ${moduleId} artifact read timed out`,
      moduleId,
      () => controller.abort(),
    );
    const actualDigest = await withinTimeout(
      this.runtime.sha256(bytes),
      timeoutMs,
      "ARTIFACT_INVALID",
      `Frontend module ${moduleId} digest verification timed out`,
      moduleId,
    );
    if (actualDigest !== expectedDigest) {
      throw new FrontendExtensionError(
        "DIGEST_MISMATCH",
        `Frontend module ${moduleId} digest verification failed`,
        moduleId,
      );
    }
    const moduleURL = this.runtime.createModuleURL(new Blob([bytes], { type: "text/javascript" }));
    try {
      return await withinTimeout(
        this.runtime.importModule(moduleURL),
        timeoutMs,
        "MODULE_INVALID",
        `Frontend module ${moduleId} import timed out`,
        moduleId,
      );
    } finally {
      this.runtime.revokeModuleURL(moduleURL);
    }
  }

  private publicHandle(record: ActiveRecordV1): ActiveExtensionV1 {
    return Object.freeze({
      moduleId: record.manifest.moduleId,
      bundleDigest: record.bundleDigest,
      manifest: record.manifest,
      dispose: () => this.unloadIfCurrent(record),
    });
  }

  private async unloadIfCurrent(record: ActiveRecordV1): Promise<void> {
    await this.serialize(record.manifest.moduleId, async () => {
      if (this.activeRecords.get(record.manifest.moduleId) !== record) return;
      this.activeRecords.delete(record.manifest.moduleId);
      await this.disposeRecord(record);
    });
  }

  private async disposeRecord(record: ActiveRecordV1): Promise<void> {
    if (record.disposed) return;
    record.disposed = true;
    await disposeOne(record.mountDisposable, record.logger, "mount", record.timeoutMs);
    await disposeActivation(record.activation, record.logger, record.timeoutMs);
    record.root.remove();
  }

  private serialize<T>(moduleId: string, action: () => Promise<T>): Promise<T> {
    const previous = this.operations.get(moduleId) ?? Promise.resolve();
    const result = previous.catch(() => undefined).then(action);
    const tail = result.then(
      () => undefined,
      () => undefined,
    );
    this.operations.set(moduleId, tail);
    return result.finally(() => {
      if (this.operations.get(moduleId) === tail) this.operations.delete(moduleId);
    });
  }
}

export function parseFrontendManifest(value: unknown, expectedTarget: FrontendTarget): FrontendManifestV1 {
  const object = exactObject(value, allowedManifestKeys, allowedManifestKeys, "frontend manifest");
  if (object.schemaVersion !== FRONTEND_SCHEMA_VERSION) {
    throw new FrontendExtensionError(
      "MANIFEST_INVALID",
      `frontend manifest schemaVersion must be ${FRONTEND_SCHEMA_VERSION}`,
    );
  }
  if (!identifierPattern.test(stringValue(object.moduleId, "moduleId"))) {
    throw new FrontendExtensionError("MANIFEST_INVALID", "frontend manifest moduleId is invalid");
  }
  if (object.target !== "user-shell" && object.target !== "admin-shell") {
    throw new FrontendExtensionError("MANIFEST_INVALID", "frontend manifest target is invalid");
  }
  if (object.target !== expectedTarget) {
    throw new FrontendExtensionError(
      "TARGET_MISMATCH",
      `frontend manifest target ${object.target} cannot load in ${expectedTarget}`,
      object.moduleId as string,
    );
  }
  const artifact = validateArtifact(stringValue(object.artifact, "artifact"), object.moduleId as string);
  const hostApiRange = stringValue(object.hostApiRange, "hostApiRange");
  if (!satisfiesSemver(FRONTEND_HOST_API_VERSION, hostApiRange)) {
    throw new FrontendExtensionError(
      "HOST_API_INCOMPATIBLE",
      `frontend module ${object.moduleId as string} requires host API ${hostApiRange}; host is ${FRONTEND_HOST_API_VERSION}`,
      object.moduleId as string,
    );
  }
  if (!Array.isArray(object.routes)) {
    throw new FrontendExtensionError("MANIFEST_INVALID", "frontend manifest routes must be an array");
  }
  const routeIDs = new Set<string>();
  const routes = object.routes.map((route, index) => {
    const routeObject = exactObject(route, allowedRouteKeys, ["id", "path", "title"], `route ${index}`);
    const id = stringValue(routeObject.id, `route ${index} id`);
    if (!identifierPattern.test(id) || routeIDs.has(id)) {
      throw new FrontendExtensionError("MANIFEST_INVALID", `frontend route id ${id} is invalid or duplicated`);
    }
    routeIDs.add(id);
    const path = validateRoutePath(stringValue(routeObject.path, `route ${id} path`));
    const title = stringValue(routeObject.title, `route ${id} title`).trim();
    if (title.length === 0 || title.length > 256) {
      throw new FrontendExtensionError("MANIFEST_INVALID", `frontend route ${id} title is invalid`);
    }
    const menu = routeObject.menu === undefined ? false : booleanValue(routeObject.menu, `route ${id} menu`);
    const order = routeObject.order === undefined ? 0 : integerValue(routeObject.order, `route ${id} order`);
    let permission: string | undefined;
    if (routeObject.permission !== undefined) {
      permission = stringValue(routeObject.permission, `route ${id} permission`);
      if (!permissionPattern.test(permission)) {
        throw new FrontendExtensionError("MANIFEST_INVALID", `frontend route ${id} permission is invalid`);
      }
    }
    return Object.freeze({ id, path, title, menu, order, permission });
  });
  return Object.freeze({
    schemaVersion: FRONTEND_SCHEMA_VERSION,
    moduleId: object.moduleId as string,
    target: object.target,
    artifact,
    hostApiRange,
    routes: Object.freeze(routes),
  });
}

export function satisfiesSemver(versionValue: string, rangeValue: string): boolean {
  const version = parseVersion(versionValue);
  if (version === undefined || typeof rangeValue !== "string") return false;
  const range = rangeValue.trim();
  if (range.length === 0 || range.length > 256) return false;
  return range.split("||").some((alternative) => {
    const normalized = alternative.trim().replace(/,/g, " ");
    if (normalized === "") return false;
    const hyphen = normalized.match(/^(\d+(?:\.\d+){0,2})\s+-\s+(\d+(?:\.\d+){0,2})$/);
    if (hyphen !== null) {
      const lower = completeVersion(hyphen[1]);
      const upper = completeVersion(hyphen[2]);
      return lower !== undefined && upper !== undefined && compareVersion(version, lower) >= 0 && compareVersion(version, upper) <= 0;
    }
    const tokens = normalized.split(/\s+/).filter(Boolean);
    return tokens.length > 0 && tokens.every((token) => matchesRangeToken(version, token));
  });
}

export async function sha256Digest(bytes: ArrayBuffer): Promise<string> {
  if (globalThis.crypto?.subtle === undefined) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", "Web Crypto SHA-256 is unavailable");
  }
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return `sha256:${[...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, "0")).join("")}`;
}

function createRuntime(overrides: Partial<FrontendLoaderRuntimeV1> | undefined): FrontendLoaderRuntimeV1 {
  const origin = canonicalOrigin(overrides?.origin ?? globalThis.location?.origin ?? "");
  const documentValue = overrides?.document ?? globalThis.document;
  if (documentValue === undefined) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", "DOM document is unavailable");
  }
  const fetchArtifact = overrides?.fetchArtifact ?? ((url: string, init: RequestInit) => fetch(url, init));
  const createModuleURL = overrides?.createModuleURL ?? ((blob: Blob) => URL.createObjectURL(blob));
  const revokeModuleURL = overrides?.revokeModuleURL ?? ((url: string) => URL.revokeObjectURL(url));
  const importModule = overrides?.importModule ?? ((url: string) => import(/* @vite-ignore */ url));
  return {
    origin,
    document: documentValue,
    fetchArtifact,
    sha256: overrides?.sha256 ?? sha256Digest,
    createModuleURL,
    revokeModuleURL,
    importModule,
  };
}

function createModuleHost(services: FrontendHostServicesV1, moduleId: string): FrontendModuleHostV1 {
  const logger = safeLogger(services.loggerFor(moduleId));
  const client = Object.freeze({
    request: <T = unknown>(operationId: string, call?: ClientCallV1): Promise<T> => {
      if (!identifierPattern.test(operationId)) {
        return Promise.reject(new Error("operationId is invalid"));
      }
      return services.client.request<T>(operationId, call);
    },
  });
  const permissions = Object.freeze({
    has: (permission: string): boolean => permissionPattern.test(permission) && services.permissions.has(permission),
    subscribe: (listener: () => void): Disposable =>
      validateDisposable(services.permissions.subscribe(listener), moduleId, "permission subscription"),
  });
  const theme = Object.freeze({
    current: (): ThemeSnapshotV1 => cloneTheme(services.theme.current()),
    subscribe: (listener: (theme: ThemeSnapshotV1) => void): Disposable =>
      validateDisposable(services.theme.subscribe((value) => listener(cloneTheme(value))), moduleId, "theme subscription"),
  });
  const i18n = Object.freeze({
    locale: (): string => services.i18n.locale(),
    translate: (key: string, values?: Readonly<Record<string, string | number>>): string =>
      services.i18n.translate(key, values),
    subscribe: (listener: (locale: string) => void): Disposable =>
      validateDisposable(services.i18n.subscribe(listener), moduleId, "i18n subscription"),
  });
  return Object.freeze({
    apiVersion: FRONTEND_HOST_API_VERSION,
    client,
    permissions,
    theme,
    i18n,
    logger,
  });
}

function validateHostServices(host: FrontendHostServicesV1): FrontendHostServicesV1 {
  if (
    host === null ||
    typeof host !== "object" ||
    typeof host.client?.request !== "function" ||
    typeof host.permissions?.has !== "function" ||
    typeof host.permissions?.subscribe !== "function" ||
    typeof host.theme?.current !== "function" ||
    typeof host.theme?.subscribe !== "function" ||
    typeof host.i18n?.locale !== "function" ||
    typeof host.i18n?.translate !== "function" ||
    typeof host.i18n?.subscribe !== "function" ||
    typeof host.loggerFor !== "function"
  ) {
    throw new FrontendExtensionError("MODULE_INVALID", "Frontend host services are incomplete");
  }
  return host;
}

function safeLogger(logger: ScopedLoggerV1): ScopedLoggerV1 {
  const noop = (): void => undefined;
  const call = (
    method: ((message: string, fields?: Readonly<Record<string, unknown>>) => void) | undefined,
    message: string,
    fields?: Readonly<Record<string, unknown>>,
  ): void => {
    try {
      method?.call(logger, message, fields);
    } catch {
      // Telemetry must never change extension lifecycle or rollback semantics.
    }
  };
  return Object.freeze({
    debug: typeof logger?.debug === "function" ? (message: string, fields?: Readonly<Record<string, unknown>>) => call(logger.debug, message, fields) : noop,
    info: typeof logger?.info === "function" ? (message: string, fields?: Readonly<Record<string, unknown>>) => call(logger.info, message, fields) : noop,
    warn: typeof logger?.warn === "function" ? (message: string, fields?: Readonly<Record<string, unknown>>) => call(logger.warn, message, fields) : noop,
    error: typeof logger?.error === "function" ? (message: string, fields?: Readonly<Record<string, unknown>>) => call(logger.error, message, fields) : noop,
  });
}

function cloneTheme(value: ThemeSnapshotV1): ThemeSnapshotV1 {
  return Object.freeze({
    mode: String(value.mode),
    variables: Object.freeze({ ...(value.variables ?? {}) }),
  });
}

function selectRoute(manifest: FrontendManifestV1, routeID: string | undefined): FrontendRouteV1 | undefined {
  if (routeID === undefined) {
    if (manifest.routes.length <= 1) return manifest.routes[0];
    throw new FrontendExtensionError(
      "MANIFEST_INVALID",
      `frontend module ${manifest.moduleId} requires an explicit routeId`,
      manifest.moduleId,
    );
  }
  const route = manifest.routes.find((candidate) => candidate.id === routeID);
  if (route === undefined) {
    throw new FrontendExtensionError(
      "MANIFEST_INVALID",
      `frontend module ${manifest.moduleId} has no route ${routeID}`,
      manifest.moduleId,
    );
  }
  return route;
}

function validateModule(namespace: unknown, moduleId: string): FrontendModuleV1 {
  if (namespace === null || typeof namespace !== "object" || typeof (namespace as { activate?: unknown }).activate !== "function") {
    throw new FrontendExtensionError("MODULE_INVALID", `frontend module ${moduleId} does not export activate`, moduleId);
  }
  return namespace as FrontendModuleV1;
}

function validateActivation(value: unknown, moduleId: string): FrontendActivationV1 {
  if (
    value === null ||
    typeof value !== "object" ||
    typeof (value as { mount?: unknown }).mount !== "function" ||
    typeof (value as { dispose?: unknown }).dispose !== "function"
  ) {
    throw new FrontendExtensionError("MODULE_INVALID", `frontend module ${moduleId} activation is invalid`, moduleId);
  }
  return value as FrontendActivationV1;
}

function validateDisposable(value: unknown, moduleId: string, label: string): Disposable {
  if (value === null || typeof value !== "object" || typeof (value as { dispose?: unknown }).dispose !== "function") {
    throw new FrontendExtensionError("MODULE_INVALID", `frontend module ${moduleId} ${label} is not disposable`, moduleId);
  }
  return value as Disposable;
}

async function disposeActivation(value: unknown, logger: ScopedLoggerV1, timeoutMs: number): Promise<void> {
  if (value !== null && typeof value === "object" && typeof (value as { dispose?: unknown }).dispose === "function") {
    await disposeOne(value as Disposable, logger, "activation", timeoutMs);
  }
}

async function disposeOne(value: unknown, logger: ScopedLoggerV1, label: string, timeoutMs: number): Promise<void> {
  if (value === null || typeof value !== "object" || typeof (value as { dispose?: unknown }).dispose !== "function") return;
  await new Promise<void>((resolve) => {
    let settled = false;
    const finish = (): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(() => {
      logger.warn(`frontend extension ${label} dispose timed out`);
      finish();
    }, timeoutMs);
    Promise.resolve()
      .then(() => (value as Disposable).dispose())
      .then(finish, (error) => {
        logger.warn(`frontend extension ${label} dispose failed`, { error: errorMessage(error) });
        finish();
      });
  });
}

function buildArtifactURL(origin: string, digest: string, artifact: string): string {
  const safeArtifact = validateArtifact(artifact, "");
  const encodedArtifact = safeArtifact.split("/").map(encodeURIComponent).join("/");
  const digestHex = digest.slice("sha256:".length);
  const result = new URL(`/__ojos/extensions/${digestHex}/${encodedArtifact}`, origin);
  if (result.origin !== origin) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", "frontend artifact URL must be same-origin");
  }
  return result.href;
}

function canonicalOrigin(value: string): string {
  try {
    const parsed = new URL(value);
    if (parsed.origin !== value || parsed.username !== "" || parsed.password !== "") throw new Error("not an origin");
    if (parsed.protocol !== "https:" && parsed.hostname !== "localhost" && parsed.hostname !== "127.0.0.1" && parsed.hostname !== "::1") {
      throw new Error("insecure origin");
    }
    return parsed.origin;
  } catch (cause) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", "Shell origin is invalid", "", cause);
  }
}

function canonicalDigest(value: string, moduleId: string): string {
  if (!digestPattern.test(value)) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", `frontend module ${moduleId} bundle digest is invalid`, moduleId);
  }
  return value;
}

function validateArtifact(value: string, moduleId: string): string {
  if (value.length === 0 || value.length > 1024 || value.startsWith("/") || value.includes("\\") || value.includes("?") || value.includes("#")) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", `frontend module ${moduleId} artifact path is invalid`, moduleId);
  }
  const segments = value.split("/");
  if (segments.some((segment) => segment === "." || segment === ".." || !artifactSegmentPattern.test(segment))) {
    throw new FrontendExtensionError("ARTIFACT_INVALID", `frontend module ${moduleId} artifact path is invalid`, moduleId);
  }
  return value;
}

function validateRoutePath(value: string): string {
  if (
    value.length === 0 ||
    value.length > 1024 ||
    !value.startsWith("/") ||
    value.startsWith("//") ||
    value.includes("?") ||
    value.includes("#") ||
    value.includes("\\") ||
    value.split("/").some((segment) => segment === "." || segment === "..")
  ) {
    throw new FrontendExtensionError("MANIFEST_INVALID", `frontend route path ${value} is invalid`);
  }
  return value;
}

function exactObject(
  value: unknown,
  allowed: readonly string[],
  required: readonly string[],
  label: string,
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new FrontendExtensionError("MANIFEST_INVALID", `${label} must be an object`);
  }
  const object = value as Record<string, unknown>;
  const keys = Object.keys(object);
  const unknown = keys.find((key) => !allowed.includes(key));
  const missing = required.find((key) => !Object.prototype.hasOwnProperty.call(object, key));
  if (unknown !== undefined || missing !== undefined) {
    throw new FrontendExtensionError(
      "MANIFEST_INVALID",
      unknown !== undefined ? `${label} contains unknown field ${unknown}` : `${label} is missing field ${missing!}`,
    );
  }
  return object;
}

function stringValue(value: unknown, label: string): string {
  if (typeof value !== "string") {
    throw new FrontendExtensionError("MANIFEST_INVALID", `${label} must be a string`);
  }
  return value;
}

function booleanValue(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") {
    throw new FrontendExtensionError("MANIFEST_INVALID", `${label} must be a boolean`);
  }
  return value;
}

function integerValue(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new FrontendExtensionError("MANIFEST_INVALID", `${label} must be an integer`);
  }
  return value;
}

function validateTimeout(value: number): number {
  if (!Number.isSafeInteger(value) || value < 1 || value > 120_000) {
    throw new FrontendExtensionError("TIMEOUT", "frontend extension timeout must be between 1 and 120000 milliseconds");
  }
  return value;
}

function isElementLike(value: unknown): value is HTMLElement {
  return value !== null && typeof value === "object" && typeof (value as { appendChild?: unknown }).appendChild === "function";
}

function normalizeLoadError(cause: unknown, moduleId: string): FrontendExtensionError {
  if (cause instanceof FrontendExtensionError) return cause;
  return new FrontendExtensionError("ACTIVATE_FAILED", `frontend module ${moduleId} failed: ${errorMessage(cause)}`, moduleId, cause);
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function withinTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  code: FrontendExtensionErrorCode,
  message: string,
  moduleId: string,
  onTimeout?: () => void,
  onLateResolve?: (value: T) => void | Promise<void>,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      settled = true;
      onTimeout?.();
      reject(new FrontendExtensionError(code === "TIMEOUT" ? code : "TIMEOUT", message, moduleId));
    }, timeoutMs);
    promise.then(
      (value) => {
        if (settled) {
          void onLateResolve?.(value);
          return;
        }
        settled = true;
        clearTimeout(timer);
        resolve(value);
      },
      (cause) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(cause instanceof FrontendExtensionError ? cause : new FrontendExtensionError(code, message, moduleId, cause));
      },
    );
  });
}

function parseVersion(value: string): ParsedVersion | undefined {
  const match = value.match(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/);
  if (match === null) return undefined;
  return { major: Number(match[1]), minor: Number(match[2]), patch: Number(match[3]) };
}

function completeVersion(value: string): ParsedVersion | undefined {
  const parsed = parsePartialVersion(value);
  if (parsed === undefined || parsed.wildcard) return undefined;
  return {
    major: parsed.values[0] ?? 0,
    minor: parsed.values[1] ?? 0,
    patch: parsed.values[2] ?? 0,
  };
}

function parsePartialVersion(value: string): PartialVersion | undefined {
  if (value === "*" || value.toLowerCase() === "x") return { values: [], wildcard: true };
  const parts = value.split(".");
  if (parts.length > 3 || parts.length === 0) return undefined;
  const values: number[] = [];
  let wildcard = false;
  for (let index = 0; index < parts.length; index += 1) {
    const part = parts[index]!;
    if (part === "*" || part.toLowerCase() === "x") {
      if (index !== parts.length - 1) return undefined;
      wildcard = true;
      continue;
    }
    if (!/^(0|[1-9]\d*)$/.test(part) || wildcard) return undefined;
    values.push(Number(part));
  }
  return { values, wildcard };
}

function matchesRangeToken(version: ParsedVersion, token: string): boolean {
  const match = token.match(/^(\^|~|>=|<=|>|<|=)?(.+)$/);
  if (match === null) return false;
  const operator = match[1] ?? "";
  const partial = parsePartialVersion(match[2]);
  if (partial === undefined) return false;
  if (partial.values.length === 0) return operator === "" || operator === "=";
  const lower: ParsedVersion = {
    major: partial.values[0] ?? 0,
    minor: partial.values[1] ?? 0,
    patch: partial.values[2] ?? 0,
  };
  if (operator === ">=" || operator === ">" || operator === "<=" || operator === "<" || operator === "=") {
    if (partial.wildcard || (operator !== "=" && partial.values.length !== 3)) return false;
    const comparison = compareVersion(version, lower);
    if (operator === ">=") return comparison >= 0;
    if (operator === ">") return comparison > 0;
    if (operator === "<=") return comparison <= 0;
    if (operator === "<") return comparison < 0;
    return comparison === 0;
  }
  if (operator === "^") {
    const upper = lower.major > 0
      ? { major: lower.major + 1, minor: 0, patch: 0 }
      : lower.minor > 0
        ? { major: 0, minor: lower.minor + 1, patch: 0 }
        : { major: 0, minor: 0, patch: lower.patch + 1 };
    return compareVersion(version, lower) >= 0 && compareVersion(version, upper) < 0;
  }
  if (operator === "~") {
    const upper = partial.values.length <= 1
      ? { major: lower.major + 1, minor: 0, patch: 0 }
      : { major: lower.major, minor: lower.minor + 1, patch: 0 };
    return compareVersion(version, lower) >= 0 && compareVersion(version, upper) < 0;
  }
  if (partial.wildcard || partial.values.length < 3) {
    const sameMajor = version.major === lower.major;
    const sameMinor = partial.values.length < 2 || version.minor === lower.minor;
    return sameMajor && sameMinor;
  }
  return compareVersion(version, lower) === 0;
}

function compareVersion(left: ParsedVersion, right: ParsedVersion): number {
  if (left.major !== right.major) return left.major - right.major;
  if (left.minor !== right.minor) return left.minor - right.minor;
  return left.patch - right.patch;
}
