import {
  parseFrontendManifest,
  type ActiveExtensionV1,
  type Disposable,
  type FrontendManifestV1,
  type FrontendRouteV1,
  type FrontendTarget,
  type InstallExtensionRequestV1,
  type PermissionHostServiceV1,
} from "./loader";

export const FRONTEND_CONTRIBUTION_SNAPSHOT_V1 =
  "ojos.dev/frontend-contribution-snapshot/v1";

export interface FrontendContributionModuleV1 {
  readonly revisionId: string;
  readonly deploymentId: string;
  readonly serviceId: string;
  readonly generation: number;
  readonly status: "ACTIVE";
  readonly manifest: FrontendManifestV1;
  readonly manifestDigest: string;
  readonly manifestReference: string;
  readonly bundleDigest: string;
  readonly bundleReference: string;
}

export interface FrontendContributionSnapshotV1 {
  readonly schemaVersion: typeof FRONTEND_CONTRIBUTION_SNAPSHOT_V1;
  readonly target: FrontendTarget;
  readonly snapshotRevision: string;
  readonly modules: readonly FrontendContributionModuleV1[];
}

export interface FrontendContributionMenuV1 {
  readonly moduleId: string;
  readonly routeId: string;
  readonly path: string;
  readonly title: string;
  readonly order: number;
  readonly permission?: string;
}

export interface FrontendContributionFailureV1 {
  readonly moduleId: string;
  readonly revisionId: string;
  readonly message: string;
}

export interface FrontendContributionStatusV1 {
  readonly snapshotRevision: string;
  readonly menus: readonly FrontendContributionMenuV1[];
  readonly failures: readonly FrontendContributionFailureV1[];
}

export type FrontendContributionFetcherV1 = (
  target: FrontendTarget,
  signal: AbortSignal,
) => Promise<unknown>;

export interface FrontendExtensionLoaderPortV1 {
  install(request: InstallExtensionRequestV1): Promise<ActiveExtensionV1>;
  mountSurface(
    moduleId: string,
    surfaceId: string,
    routeId: string,
    routeContext?: Readonly<Record<string, unknown>>,
  ): Promise<ActiveExtensionV1>;
  unload(moduleId: string): Promise<void>;
  dispose(): Promise<void>;
}

export interface FrontendRouteViewV1 {
  mount(
    element: HTMLElement,
    routeContext: Readonly<Record<string, unknown>>,
  ): Promise<void>;
  unmount(element: HTMLElement): void;
}

export interface FrontendDynamicRouteAdapterV1 {
  validate(moduleId: string, routes: readonly FrontendRouteV1[]): void;
  register(
    moduleId: string,
    route: FrontendRouteV1,
    view: FrontendRouteViewV1,
  ): Disposable;
}

export interface FrontendContributionHostOptionsV1 {
  readonly target: FrontendTarget;
  readonly loader: FrontendExtensionLoaderPortV1;
  readonly permissions: PermissionHostServiceV1;
  readonly routes: FrontendDynamicRouteAdapterV1;
  readonly fetchSnapshot: FrontendContributionFetcherV1;
  readonly document?: Document;
  readonly pollIntervalMs?: number;
  readonly onFailure?: (failure: FrontendContributionFailureV1) => void;
}

interface ActiveModuleRecordV1 {
  spec: FrontendContributionModuleV1;
  identity: string;
  container: HTMLElement;
  routeDisposables: Map<string, Disposable>;
  permittedRoutes: readonly FrontendRouteV1[];
  mountedRouteId: string;
  routeContextIdentity: string;
  attachedElement?: HTMLElement;
}

const snapshotKeys = ["schemaVersion", "target", "snapshotRevision", "modules"];
const moduleKeys = [
  "revisionId",
  "deploymentId",
  "serviceId",
  "generation",
  "status",
  "manifest",
  "manifestDigest",
  "manifestReference",
  "bundleDigest",
  "bundleReference",
];
const digestPattern = /^sha256:[0-9a-f]{64}$/;
const identifierPattern = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}$/;

export class FrontendContributionHost {
  private readonly target: FrontendTarget;
  private readonly loader: FrontendExtensionLoaderPortV1;
  private readonly permissions: PermissionHostServiceV1;
  private readonly routes: FrontendDynamicRouteAdapterV1;
  private readonly fetchSnapshot: FrontendContributionFetcherV1;
  private readonly stagingRoot: HTMLElement;
  private readonly pollIntervalMs: number;
  private readonly onFailure?: (failure: FrontendContributionFailureV1) => void;
  private readonly active = new Map<string, ActiveModuleRecordV1>();
  private readonly listeners = new Set<(status: FrontendContributionStatusV1) => void>();
  private desired?: FrontendContributionSnapshotV1;
  private statusValue: FrontendContributionStatusV1 = Object.freeze({
    snapshotRevision: "",
    menus: Object.freeze([]),
    failures: Object.freeze([]),
  });
  private queue: Promise<void> = Promise.resolve();
  private timer?: ReturnType<typeof setInterval>;
  private fetchController?: AbortController;
  private permissionSubscription?: Disposable;
  private started = false;
  private disposed = false;

  constructor(options: FrontendContributionHostOptionsV1) {
    this.target = options.target;
    this.loader = options.loader;
    this.permissions = options.permissions;
    this.routes = options.routes;
    this.fetchSnapshot = options.fetchSnapshot;
    this.pollIntervalMs = validPollInterval(options.pollIntervalMs ?? 30_000);
    this.onFailure = options.onFailure;
    const ownerDocument = options.document ?? globalThis.document;
    if (ownerDocument === undefined) {
      throw new Error("frontend contribution host requires a DOM document");
    }
    this.stagingRoot = ownerDocument.createElement("div");
    this.stagingRoot.hidden = true;
    this.stagingRoot.dataset.ojosFrontendStaging = this.target;
  }

  status(): FrontendContributionStatusV1 {
    return this.statusValue;
  }

  subscribe(listener: (status: FrontendContributionStatusV1) => void): Disposable {
    this.listeners.add(listener);
    listener(this.statusValue);
    return { dispose: () => void this.listeners.delete(listener) };
  }

  async start(): Promise<void> {
    if (this.disposed) throw new Error("frontend contribution host is disposed");
    if (this.started) return;
    this.started = true;
    this.permissionSubscription = this.permissions.subscribe(() => {
      if (this.desired !== undefined) {
        void this.enqueue(() => this.reconcileParsed(this.desired!));
      }
    });
    await this.refresh();
    if (!this.disposed) {
      this.timer = setInterval(() => void this.refresh(), this.pollIntervalMs);
    }
  }

  async refresh(): Promise<FrontendContributionStatusV1> {
    return this.enqueue(async () => {
      if (this.disposed) return;
      this.fetchController?.abort("snapshot superseded");
      const controller = new AbortController();
      this.fetchController = controller;
      try {
        const raw = await this.fetchSnapshot(this.target, controller.signal);
        const snapshot = parseFrontendContributionSnapshot(raw, this.target);
        this.desired = snapshot;
        await this.reconcileParsed(snapshot);
      } catch (error) {
        if (!controller.signal.aborted) {
          this.reportFailure("", "", error);
          this.publish(
            this.statusValue.snapshotRevision,
            this.statusValue.menus,
            [failure("", "", error)],
          );
        }
      } finally {
        if (this.fetchController === controller) this.fetchController = undefined;
      }
    }).then(() => this.statusValue);
  }

  async reconcile(raw: unknown): Promise<FrontendContributionStatusV1> {
    const snapshot = parseFrontendContributionSnapshot(raw, this.target);
    return this.enqueue(async () => {
      this.desired = snapshot;
      await this.reconcileParsed(snapshot);
    }).then(() => this.statusValue);
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.timer !== undefined) clearInterval(this.timer);
    this.timer = undefined;
    this.fetchController?.abort("frontend contribution host disposed");
    this.permissionSubscription?.dispose();
    this.permissionSubscription = undefined;
    await this.enqueue(async () => {
      for (const moduleId of [...this.active.keys()].sort()) {
        await this.removeModule(moduleId);
      }
      await this.loader.dispose();
      this.publish("", [], []);
      this.listeners.clear();
      this.stagingRoot.remove();
    });
  }

  private async reconcileParsed(snapshot: FrontendContributionSnapshotV1): Promise<void> {
    const failures: FrontendContributionFailureV1[] = [];
    const desiredIDs = new Set(snapshot.modules.map((module) => module.manifest.moduleId));
    for (const moduleId of [...this.active.keys()].sort()) {
      if (!desiredIDs.has(moduleId)) await this.removeModule(moduleId);
    }
    for (const module of snapshot.modules) {
      try {
        await this.reconcileModule(module);
      } catch (error) {
        const item = failure(module.manifest.moduleId, module.revisionId, error);
        failures.push(item);
        this.reportFailure(item.moduleId, item.revisionId, error);
      }
    }
    this.publish(snapshot.snapshotRevision, this.collectMenus(), failures);
  }

  private async reconcileModule(spec: FrontendContributionModuleV1): Promise<void> {
    const moduleId = spec.manifest.moduleId;
    const existing = this.active.get(moduleId);
    const permittedRoutes = spec.manifest.routes.filter(
      (route) => route.permission === undefined || this.permissions.has(route.permission),
    );
    if (permittedRoutes.length === 0) {
      if (existing !== undefined) await this.removeModule(moduleId);
      return;
    }
    this.routes.validate(moduleId, permittedRoutes);
    const identity = moduleIdentity(spec);
    if (existing !== undefined && existing.identity === identity) {
      const nextRoute = permittedRoutes.some((route) => route.id === existing.mountedRouteId)
        ? existing.mountedRouteId
        : permittedRoutes[0]!.id;
      if (nextRoute !== existing.mountedRouteId) {
        await this.installRoute(existing, spec, nextRoute, routeContext(spec, nextRoute));
      }
      existing.spec = spec;
      existing.permittedRoutes = Object.freeze([...permittedRoutes]);
      this.replaceRouteRegistrations(existing);
      return;
    }

    const container = existing?.container ?? this.stagingRoot.ownerDocument.createElement("div");
    container.dataset.ojosFrontendHost = moduleId;
    if (existing === undefined) {
      container.hidden = true;
      this.stagingRoot.appendChild(container);
    }
    const routeID =
      existing !== undefined && permittedRoutes.some((route) => route.id === existing.mountedRouteId)
        ? existing.mountedRouteId
        : permittedRoutes[0]!.id;
    try {
      await this.loader.install({
        manifest: spec.manifest,
        bundleDigest: spec.bundleDigest,
        container,
        surfaceId: routeID,
        routeId: routeID,
        routeContext: routeContext(spec, routeID),
      });
    } catch (error) {
      if (existing === undefined) container.remove();
      throw error;
    }

    const record: ActiveModuleRecordV1 = {
      spec,
      identity,
      container,
      routeDisposables: existing?.routeDisposables ?? new Map(),
      permittedRoutes: Object.freeze([...permittedRoutes]),
      mountedRouteId: routeID,
      routeContextIdentity: JSON.stringify(routeContext(spec, routeID)),
      attachedElement: existing?.attachedElement,
    };
    this.active.set(moduleId, record);
    this.replaceRouteRegistrations(record);
    if (record.attachedElement !== undefined) {
      container.hidden = false;
      record.attachedElement.replaceChildren(container);
    }
  }

  private replaceRouteRegistrations(record: ActiveModuleRecordV1): void {
    const next = new Map<string, Disposable>();
    try {
      for (const route of record.permittedRoutes) {
        const disposable = this.routes.register(record.spec.manifest.moduleId, route, {
          mount: (element, context) =>
            this.enqueue(() => this.attach(record.spec.manifest.moduleId, route.id, element, context)),
          unmount: (element) => this.detach(record.spec.manifest.moduleId, element),
        });
        next.set(route.id, disposable);
      }
    } catch (error) {
      for (const disposable of next.values()) disposable.dispose();
      throw error;
    }
    for (const disposable of record.routeDisposables.values()) disposable.dispose();
    record.routeDisposables = next;
  }

  private async attach(
    moduleId: string,
    routeId: string,
    element: HTMLElement,
    context: Readonly<Record<string, unknown>>,
  ): Promise<void> {
    const record = this.active.get(moduleId);
    if (record === undefined) throw new Error(`frontend module ${moduleId} is unavailable`);
    if (!record.permittedRoutes.some((route) => route.id === routeId)) {
      throw new Error(`frontend route ${routeId} is unavailable`);
    }
    const nextContext = routeContext(record.spec, routeId, context);
    const nextContextIdentity = JSON.stringify(nextContext);
    if (
      record.mountedRouteId !== routeId ||
      record.attachedElement !== element ||
      record.routeContextIdentity !== nextContextIdentity
    ) {
      await this.installRoute(record, record.spec, routeId, nextContext);
      record.routeContextIdentity = nextContextIdentity;
    }
    record.attachedElement = element;
    record.container.hidden = false;
    element.replaceChildren(record.container);
  }

  private detach(moduleId: string, element: HTMLElement): void {
    const record = this.active.get(moduleId);
    if (record === undefined || record.attachedElement !== element) return;
    record.attachedElement = undefined;
    record.container.hidden = true;
    this.stagingRoot.appendChild(record.container);
  }

  private async installRoute(
    record: ActiveModuleRecordV1,
    spec: FrontendContributionModuleV1,
    routeId: string,
    context: Readonly<Record<string, unknown>>,
  ): Promise<void> {
    await this.loader.mountSurface(spec.manifest.moduleId, routeId, routeId, context);
    record.mountedRouteId = routeId;
  }

  private async removeModule(moduleId: string): Promise<void> {
    const record = this.active.get(moduleId);
    if (record === undefined) return;
    this.active.delete(moduleId);
    for (const disposable of record.routeDisposables.values()) disposable.dispose();
    record.routeDisposables.clear();
    await this.loader.unload(moduleId);
    record.container.remove();
  }

  private collectMenus(): readonly FrontendContributionMenuV1[] {
    return Object.freeze(
      [...this.active.values()]
        .flatMap((record) =>
          record.permittedRoutes
            .filter((route) => route.menu)
            .map((route) => ({
              moduleId: record.spec.manifest.moduleId,
              routeId: route.id,
              path: route.path,
              title: route.title,
              order: route.order,
              ...(route.permission === undefined ? {} : { permission: route.permission }),
            })),
        )
        .sort((left, right) =>
          left.order - right.order ||
          left.path.localeCompare(right.path) ||
          left.moduleId.localeCompare(right.moduleId),
        ),
    );
  }

  private publish(
    snapshotRevision: string,
    menus: readonly FrontendContributionMenuV1[],
    failures: readonly FrontendContributionFailureV1[],
  ): void {
    this.statusValue = Object.freeze({
      snapshotRevision,
      menus: Object.freeze([...menus]),
      failures: Object.freeze([...failures]),
    });
    for (const listener of this.listeners) listener(this.statusValue);
  }

  private reportFailure(moduleId: string, revisionId: string, error: unknown): void {
    this.onFailure?.(failure(moduleId, revisionId, error));
  }

  private enqueue(action: () => void | Promise<void>): Promise<void> {
    const result = this.queue.catch(() => undefined).then(action);
    this.queue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }
}

export function parseFrontendContributionSnapshot(
  value: unknown,
  expectedTarget: FrontendTarget,
): FrontendContributionSnapshotV1 {
  const object = exactObject(value, snapshotKeys, snapshotKeys, "frontend contribution snapshot");
  if (object.schemaVersion !== FRONTEND_CONTRIBUTION_SNAPSHOT_V1) {
    throw new Error(`frontend contribution snapshot schemaVersion must be ${FRONTEND_CONTRIBUTION_SNAPSHOT_V1}`);
  }
  if (object.target !== expectedTarget) {
    throw new Error(`frontend contribution snapshot target must be ${expectedTarget}`);
  }
  const snapshotRevision = boundedText(object.snapshotRevision, "snapshotRevision");
  if (!Array.isArray(object.modules)) throw new Error("frontend contribution snapshot modules must be an array");
  const moduleIDs = new Set<string>();
  const modules = object.modules.map((value, index) => {
    const item = exactObject(value, moduleKeys, moduleKeys, `frontend contribution module ${index}`);
    const revisionId = boundedIdentifier(item.revisionId, "revisionId");
    const deploymentId = boundedIdentifier(item.deploymentId, "deploymentId");
    const serviceId = boundedIdentifier(item.serviceId, "serviceId");
    if (!Number.isSafeInteger(item.generation) || (item.generation as number) < 1) {
      throw new Error(`frontend contribution module ${revisionId} generation is invalid`);
    }
    if (item.status !== "ACTIVE") {
      throw new Error(`frontend contribution module ${revisionId} is not ACTIVE`);
    }
    const manifest = parseFrontendManifest(item.manifest, expectedTarget);
    if (moduleIDs.has(manifest.moduleId)) {
      throw new Error(`frontend contribution module ${manifest.moduleId} is duplicated`);
    }
    moduleIDs.add(manifest.moduleId);
    const manifestDigest = boundedText(item.manifestDigest, "manifestDigest");
    if (!digestPattern.test(manifestDigest)) {
      throw new Error(`frontend contribution module ${manifest.moduleId} manifest digest is invalid`);
    }
    const bundleDigest = boundedText(item.bundleDigest, "bundleDigest");
    if (!digestPattern.test(bundleDigest)) throw new Error(`frontend contribution module ${manifest.moduleId} digest is invalid`);
    const manifestReference = contentAddressedReference(
      item.manifestReference,
      manifestDigest,
      `frontend contribution module ${manifest.moduleId} manifestReference`,
    );
    const bundleReference = contentAddressedReference(
      item.bundleReference,
      bundleDigest,
      `frontend contribution module ${manifest.moduleId} bundleReference`,
    );
    return Object.freeze({
      revisionId,
      deploymentId,
      serviceId,
      generation: item.generation as number,
      status: "ACTIVE" as const,
      manifest,
      manifestDigest,
      manifestReference,
      bundleDigest,
      bundleReference,
    });
  });
  modules.sort((left, right) => left.manifest.moduleId.localeCompare(right.manifest.moduleId));
  return Object.freeze({
    schemaVersion: FRONTEND_CONTRIBUTION_SNAPSHOT_V1,
    target: expectedTarget,
    snapshotRevision,
    modules: Object.freeze(modules),
  });
}

function moduleIdentity(spec: FrontendContributionModuleV1): string {
  return JSON.stringify([
    spec.revisionId,
    spec.deploymentId,
    spec.generation,
    spec.manifestDigest,
    spec.manifestReference,
    spec.bundleDigest,
    spec.bundleReference,
    spec.manifest,
  ]);
}

function routeContext(
  spec: FrontendContributionModuleV1,
  routeId: string,
  location: Readonly<Record<string, unknown>> = {},
): Readonly<Record<string, unknown>> {
  return Object.freeze({
    ...location,
    routeId,
    revisionId: spec.revisionId,
    deploymentId: spec.deploymentId,
    generation: spec.generation,
  });
}

function failure(moduleId: string, revisionId: string, error: unknown): FrontendContributionFailureV1 {
  return Object.freeze({
    moduleId,
    revisionId,
    message: error instanceof Error ? error.message : String(error),
  });
}

function validPollInterval(value: number): number {
  if (!Number.isSafeInteger(value) || value < 100 || value > 3_600_000) {
    throw new Error("frontend contribution poll interval is invalid");
  }
  return value;
}

function boundedText(value: unknown, label: string): string {
  if (typeof value !== "string" || value.trim() !== value || value.length < 1 || value.length > 512) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function boundedIdentifier(value: unknown, label: string): string {
  const text = boundedText(value, label);
  if (!identifierPattern.test(text)) throw new Error(`${label} is invalid`);
  return text;
}

function contentAddressedReference(value: unknown, digest: string, label: string): string {
  const reference = boundedText(value, label);
  let parsed: URL;
  try {
    parsed = new URL(reference);
  } catch {
    throw new Error(`${label} is invalid`);
  }
  if (
    parsed.protocol !== "https:" ||
    parsed.username !== "" ||
    parsed.password !== "" ||
    !parsed.pathname.toLowerCase().includes(digest.slice("sha256:".length))
  ) {
    throw new Error(`${label} is not an HTTPS content-addressed reference`);
  }
  return reference;
}

function exactObject(
  value: unknown,
  allowed: readonly string[],
  required: readonly string[],
  label: string,
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const object = value as Record<string, unknown>;
  for (const key of Object.keys(object)) {
    if (!allowed.includes(key)) throw new Error(`${label} contains unknown field ${key}`);
  }
  for (const key of required) {
    if (!(key in object)) throw new Error(`${label} is missing ${key}`);
  }
  return object;
}
