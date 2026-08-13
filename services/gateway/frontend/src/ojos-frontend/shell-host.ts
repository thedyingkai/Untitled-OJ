import type { Pinia } from "pinia";
import { shallowRef } from "vue";
import type { Router } from "vue-router";
import { apiClient } from "../api/client";
import { useAuthStore } from "../stores/auth";
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

const emptyStatus: FrontendContributionStatusV1 = Object.freeze({
  snapshotRevision: "",
  menus: Object.freeze([]),
  failures: Object.freeze([]),
});

export const userFrontendContributions = shallowRef(emptyStatus);

export interface UserFrontendHostHandleV1 extends Disposable {
  readonly host: FrontendContributionHost;
}

export function startUserFrontendHost(
  router: Router,
  pinia: Pinia,
): UserFrontendHostHandleV1 {
  const auth = useAuthStore(pinia);
  const permissions: PermissionHostServiceV1 = {
    has: (permission) => auth.hasPermission(permission),
    subscribe: (listener) => ({ dispose: auth.$subscribe(listener) }),
  };
  const operations = new OperationRouteRegistry();
  const hostServices: FrontendHostServicesV1 = Object.freeze({
    client: {
      request: <T>(operationId: string, call: ClientCallV1 = {}) => {
        const route = operations.resolve(operationId);
        return apiClient.request<T>({
          // Contribution routes are already absolute Gateway virtual paths
          // (for example /api/contests). Override apiClient's /api base to
          // avoid materializing /api/api/contests.
          baseURL: "",
          method: route.method,
          url: materializeOperationPath(route, call),
          data: call.body,
          signal: call.signal,
          headers: call.idempotencyKey === undefined
            ? undefined
            : { "Idempotency-Key": call.idempotencyKey },
        });
      },
    },
    permissions,
    theme: browserThemeService(),
    i18n: browserI18nService(),
    loggerFor: scopedConsoleLogger,
  });
  const loader = new FrontendExtensionLoader({ target: "user-shell", host: hostServices });
  const host = new FrontendContributionHost({
    target: "user-shell",
    loader,
    permissions,
    routes: createVueRouteAdapter(router, "app-shell"),
    fetchSnapshot: createContributionSnapshotFetcher(
      "user-shell",
      (signal) => apiClient.get("/v1/contributions/snapshot", { signal }),
      operations,
    ),
    onFailure: (failure) =>
      console.error("[ojos.frontend:user-shell] contribution rejected", failure),
  });
  const statusSubscription = host.subscribe((status) => {
    userFrontendContributions.value = status;
  });
  const authSubscription = auth.$subscribe(() => {
    if (auth.isAuthenticated) void host.refresh();
    else operations.clear();
  });
  void host.start().catch((error) =>
    console.error("[ojos.frontend:user-shell] startup failed", error),
  );
  let disposed = false;
  return Object.freeze({
    host,
    async dispose() {
      if (disposed) return;
      disposed = true;
      statusSubscription.dispose();
      authSubscription();
      operations.clear();
      await host.dispose();
      userFrontendContributions.value = emptyStatus;
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
