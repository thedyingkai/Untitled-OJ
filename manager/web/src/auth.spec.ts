import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  authMode,
  authRedirecting,
  authenticated,
  installBrowserAuthentication,
  markAuthRequired,
  oidcLoginUrl,
  principalId,
  resetAuthenticationForTest,
  setAuthenticationNavigatorForTest,
} from "./auth";

function v1(data: unknown): Response {
  return new Response(
    JSON.stringify({
      data,
      meta: { request_id: "req-auth", api_version: "v1" },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  );
}

describe("HttpOnly browser authentication", () => {
  beforeEach(() => {
    resetAuthenticationForTest();
    delete window.__OJOS_AUTH_READY__;
    delete window.__OJOS_CSRF_TOKEN__;
    vi.restoreAllMocks();
  });

  it("discovers an existing OIDC session and keeps only CSRF in memory", async () => {
    const storageRead = vi.spyOn(Storage.prototype, "getItem");
    const storageWrite = vi.spyOn(Storage.prototype, "setItem");
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          v1({
            mode: "oidc",
            issuer: "https://issuer.example",
            client_id: "orchestrator-web",
            audience: "orchestrator-api",
            scopes: ["openid", "profile"],
            authorization_endpoint: "https://issuer.example/authorize",
            start_url: "/api/v1/auth/oidc/start",
          }),
        )
        .mockResolvedValueOnce(
          v1({
            authenticated: true,
            principal_id: "user-123",
            role: "orchestrator.admin",
            csrf_token: "csrf-memory-only",
          }),
        ),
    );

    await installBrowserAuthentication();

    expect(authMode.value).toBe("oidc");
    expect(authenticated.value).toBe(true);
    expect(principalId.value).toBe("user-123");
    expect(window.__OJOS_CSRF_TOKEN__).toBe("csrf-memory-only");
    expect(storageRead).not.toHaveBeenCalled();
    expect(storageWrite).not.toHaveBeenCalled();
  });

  it("turns a 401 into one same-origin OIDC start redirect", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          v1({
            mode: "oidc",
            issuer: "https://issuer.example",
            client_id: "orchestrator-web",
            audience: "orchestrator-api",
            scopes: ["openid"],
            authorization_endpoint: "https://issuer.example/authorize",
            start_url: "/api/v1/auth/oidc/start",
          }),
        )
        .mockResolvedValueOnce(v1({ authenticated: false })),
    );
    await installBrowserAuthentication();
    const navigations: string[] = [];
    setAuthenticationNavigatorForTest((target) => navigations.push(target));

    markAuthRequired();
    markAuthRequired();

    expect(authRedirecting.value).toBe(true);
    expect(navigations).toHaveLength(1);
    expect(navigations[0]).toBe(oidcLoginUrl());
    expect(navigations[0]).toMatch(/^\/api\/v1\/auth\/oidc\/start\?return_to=/);
  });

  it("does not anonymously authenticate when identity mode is unknown", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(v1({ mode: "unconfigured" })));
    await installBrowserAuthentication();
    expect(authMode.value).toBe("unconfigured");
    expect(authenticated.value).toBe(false);
    expect(oidcLoginUrl()).toBeNull();
  });

  it("preserves the Desktop bootstrap readiness promise without discovery", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    window.__OJOS_AUTH_READY__ = Promise.resolve();

    await installBrowserAuthentication();

    expect(authMode.value).toBe("desktop");
    expect(authenticated.value).toBe(true);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
