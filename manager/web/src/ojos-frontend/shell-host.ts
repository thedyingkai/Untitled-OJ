import { shallowRef, watch } from "vue";
import type { Router } from "vue-router";
import { authenticated } from "../auth";
import { request, v1Request } from "../api";
import {
  FrontendContributionHost,
  type FrontendContributionStatusV1,
} from "./contribution-host";
import {
  FrontendExtensionLoader,
  type ClientCallV1,
  type Disposable,
  type FrontendHostServicesV1,
  type I18nHostServiceV1,
  type PermissionHostServiceV1,
  type ScopedLoggerV1,
  type ThemeHostServiceV1,
} from "./loader";
import {
  createContributionSnapshotFetcher,
  materializeOperationPath,
  OperationRouteRegistry,
} from "./snapshot-adapter";
import { createVueRouteAdapter } from "./vue-route-adapter";
import {
  rolePermissionQuery,
  type AdminPermissionQueryV1,
} from "./permission-query";

export { rolePermissionQuery } from "./permission-query";
export type { AdminPermissionQueryV1 } from "./permission-query";

const emptyStatus: FrontendContributionStatusV1 = Object.freeze({
  snapshotRevision: "",
  menus: Object.freeze([]),
  failures: Object.freeze([]),
});

export const adminFrontendContributions = shallowRef(emptyStatus);

export interface AdminFrontendHostHandleV1 extends Disposable {
  readonly host: FrontendContributionHost;
}

export function startAdminFrontendHost(
  router: Router,
  permissionQuery: AdminPermissionQueryV1 = rolePermissionQuery(),
): AdminFrontendHostHandleV1 {
  const permissions: PermissionHostServiceV1 = {
    has: (permission) => authenticated.value && permissionQuery.current(permission),
    subscribe: (listener) => permissionQuery.subscribe(listener),
  };
  const operations = new OperationRouteRegistry();
  const hostServices: FrontendHostServicesV1 = Object.freeze({
    client: {
      request: <T>(operationId: string, call: ClientCallV1 = {}) => {
        const route = operations.resolve(operationId);
        return request<T>(
          route.method,
          materializeOperationPath(route, call),
          call.body,
          { signal: call.signal, idempotencyKey: call.idempotencyKey },
        );
      },
    },
    permissions,
    theme: browserThemeService(),
    i18n: browserI18nService(),
    loggerFor: scopedConsoleLogger,
  });
  const loader = new FrontendExtensionLoader({ target: "admin-shell", host: hostServices });
  const host = new FrontendContributionHost({
    target: "admin-shell",
    loader,
    permissions,
    routes: createVueRouteAdapter(router),
    fetchSnapshot: createContributionSnapshotFetcher(
      "admin-shell",
      (signal) =>
        v1Request("GET", "/api/v1/contributions/snapshot", undefined, {
          signal,
        }),
      operations,
      (snapshot) => {
        // Snapshot changes clear the permission cache synchronously. Let the
        // Host reconcile that denied state before the Auth round-trip finishes;
        // the permission subscription installs allowed modules atomically once
        // the complete batch has been validated.
        void permissionQuery.replaceSnapshot(snapshot).catch((error) =>
          console.error(
            "[ojos.frontend:admin-shell] permission snapshot rejected",
            error,
          ),
        );
      },
    ),
    onFailure: (failure) =>
      console.error("[ojos.frontend:admin-shell] contribution rejected", failure),
  });
  const statusSubscription = host.subscribe((status) => {
    adminFrontendContributions.value = status;
  });
  const stopAuthRefresh = watch(authenticated, (isAuthenticated) => {
    if (isAuthenticated) void host.refresh();
    else operations.clear();
  });
  void host.start().catch((error) =>
    console.error("[ojos.frontend:admin-shell] startup failed", error),
  );
  let disposed = false;
  return Object.freeze({
    host,
    async dispose() {
      if (disposed) return;
      disposed = true;
      statusSubscription.dispose();
      stopAuthRefresh();
      operations.clear();
      await host.dispose();
      adminFrontendContributions.value = emptyStatus;
    },
  });
}

function browserThemeService(): ThemeHostServiceV1 {
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  const current = () => Object.freeze({
    mode: query?.matches ? "dark" : "light",
    variables: Object.freeze({}),
  });
  return {
    current,
    subscribe(listener) {
      const changed = () => listener(current());
      query?.addEventListener("change", changed);
      return { dispose: () => query?.removeEventListener("change", changed) };
    },
  };
}

function browserI18nService(): I18nHostServiceV1 {
  return {
    locale: () => document.documentElement.lang || navigator.language || "en",
    translate(key, values = {}) {
      return Object.entries(values).reduce(
        (result, [name, value]) => result.replaceAll(`{${name}}`, String(value)),
        key,
      );
    },
    subscribe: () => ({ dispose: () => undefined }),
  };
}

function scopedConsoleLogger(moduleId: string): ScopedLoggerV1 {
  const prefix = `[ojos.frontend:${moduleId}]`;
  return {
    debug: (message, fields) => console.debug(prefix, message, fields ?? {}),
    info: (message, fields) => console.info(prefix, message, fields ?? {}),
    warn: (message, fields) => console.warn(prefix, message, fields ?? {}),
    error: (message, fields) => console.error(prefix, message, fields ?? {}),
  };
}
