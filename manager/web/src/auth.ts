import { computed, ref } from "vue";

declare global {
  interface Window {
    __OJOS_AUTH_READY__?: Promise<void>;
    __OJOS_CSRF_TOKEN__?: string;
  }
}

export type BrowserAuthMode =
  | "initializing"
  | "desktop"
  | "oidc"
  | "development"
  | "unconfigured"
  | "unavailable";

interface AuthConfig {
  mode: BrowserAuthMode;
  issuer?: string;
  client_id?: string;
  audience?: string;
  scopes?: string[];
  authorization_endpoint?: string;
  start_url?: string;
}

interface AuthSession {
  authenticated: boolean;
  principal_id?: string;
  role?: string;
  csrf_token?: string | null;
}

interface V1Envelope<T> {
  data: T;
  meta: { request_id: string; api_version: string };
}

class AuthHttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "AuthHttpError";
  }
}

export const authMode = ref<BrowserAuthMode>("initializing");
export const authenticated = ref(false);
export const authRequired = ref(false);
export const authRedirecting = ref(false);
export const authError = ref("");
export const principalId = ref("");
export const principalRole = ref("");
export const authLabel = computed(() => {
  if (authMode.value === "desktop") return "Desktop 本地会话";
  if (authenticated.value && principalId.value) {
    return `${principalId.value}${principalRole.value ? ` · ${principalRole.value}` : ""}`;
  }
  if (authMode.value === "oidc") return "等待 OIDC 登录";
  if (authMode.value === "development") return "开发身份";
  return "身份未配置";
});

let config: AuthConfig | null = null;
let installed = false;
let navigateTo = (target: string) => window.location.assign(target);

async function rawV1<T>(path: string): Promise<T> {
  const response = await fetch(path, {
    method: "GET",
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  const body = (await response.json()) as Partial<V1Envelope<T>> & {
    detail?: string;
  };
  if (!response.ok) {
    throw new AuthHttpError(body.detail || `HTTP ${response.status}`, response.status);
  }
  if (!body.data || !body.meta?.request_id) {
    throw new Error(`身份接口返回了无效的 v1 envelope：${path}`);
  }
  return body.data;
}

async function initializeRemoteAuthentication(): Promise<void> {
  try {
    config = await rawV1<AuthConfig>("/api/v1/auth/config");
    authMode.value = config.mode;
    if (config.mode === "development") {
      authenticated.value = true;
      return;
    }
    if (config.mode !== "oidc") {
      authenticated.value = false;
      return;
    }
    let session: AuthSession;
    try {
      session = await rawV1<AuthSession>("/api/v1/auth/session");
    } catch (error) {
      if (error instanceof AuthHttpError && error.status === 401) {
        authenticated.value = false;
        window.__OJOS_CSRF_TOKEN__ = "";
        return;
      }
      throw error;
    }
    authenticated.value = session.authenticated === true;
    principalId.value = session.principal_id ?? "";
    principalRole.value = session.role ?? "";
    window.__OJOS_CSRF_TOKEN__ = session.csrf_token?.trim() ?? "";
  } catch (error) {
    authMode.value = "unavailable";
    authenticated.value = false;
    authError.value = `无法读取编排器身份配置：${
      error instanceof Error ? error.message : String(error)
    }`;
  }
}

/**
 * Installs exactly one authentication readiness promise. Desktop injects its
 * own bootstrap exchange before the bundle runs; remote Web discovers OIDC
 * without ever reading or persisting a bearer token.
 */
export function installBrowserAuthentication(): Promise<void> {
  if (installed && window.__OJOS_AUTH_READY__) return window.__OJOS_AUTH_READY__;
  installed = true;
  const desktopBootstrap = window.__OJOS_AUTH_READY__;
  if (desktopBootstrap) {
    authMode.value = "desktop";
    const ready = desktopBootstrap.then(() => {
      authenticated.value = true;
      authRequired.value = false;
    });
    window.__OJOS_AUTH_READY__ = ready;
    return ready;
  }
  const ready = initializeRemoteAuthentication();
  window.__OJOS_AUTH_READY__ = ready;
  return ready;
}

export function oidcLoginUrl(): string | null {
  if (authMode.value !== "oidc" || !config?.start_url) return null;
  const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  const separator = config.start_url.includes("?") ? "&" : "?";
  return `${config.start_url}${separator}return_to=${encodeURIComponent(returnTo || "/")}`;
}

export function beginOidcLogin(): void {
  if (authRedirecting.value) return;
  const target = oidcLoginUrl();
  if (!target) return;
  authRedirecting.value = true;
  navigateTo(target);
}

export function markAuthRequired(): void {
  authenticated.value = false;
  authRequired.value = true;
  window.__OJOS_CSRF_TOKEN__ = "";
  beginOidcLogin();
}

export function resolveAuthRequired(): void {
  authRequired.value = false;
  authRedirecting.value = false;
}

export async function logoutBrowserSession(): Promise<void> {
  if (authMode.value !== "oidc") return;
  const response = await fetch("/api/v1/auth/logout", {
    method: "POST",
    credentials: "same-origin",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
      "x-csrf-token": window.__OJOS_CSRF_TOKEN__ ?? "",
      "Idempotency-Key": crypto.randomUUID(),
    },
    body: "{}",
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new Error(body.detail || `退出登录失败：HTTP ${response.status}`);
  }
  authenticated.value = false;
  principalId.value = "";
  principalRole.value = "";
  window.__OJOS_CSRF_TOKEN__ = "";
  authRequired.value = true;
  beginOidcLogin();
}

export function resetAuthenticationForTest(): void {
  installed = false;
  config = null;
  authMode.value = "initializing";
  authenticated.value = false;
  authRequired.value = false;
  authRedirecting.value = false;
  authError.value = "";
  principalId.value = "";
  principalRole.value = "";
  navigateTo = (target) => window.location.assign(target);
}

export function setAuthenticationNavigatorForTest(
  navigator: (target: string) => void,
): void {
  navigateTo = navigator;
}
