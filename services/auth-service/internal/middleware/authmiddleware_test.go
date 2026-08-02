package middleware

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestAuthMiddlewareCarriesOpaqueServiceCredentialIntoPermissionCheck(t *testing.T) {
	middleware := NewAuthMiddleware("jwt-secret", "")
	handler := middleware.Handle(func(w http.ResponseWriter, r *http.Request) {
		claims, ok := ClaimsFromContext(r.Context())
		if !ok || claims == nil || len(claims.Roles) != 1 || claims.Roles[0] != "service" {
			t.Fatalf("expected service claims, got %#v", claims)
		}
		if got, ok := TokenFromContext(r.Context()); !ok || got != "service-token" {
			t.Fatalf("expected opaque token in context, got %q ok=%v", got, ok)
		}
		w.WriteHeader(http.StatusNoContent)
	})

	req := httptest.NewRequest(http.MethodPost, "/auth/permission-check", nil)
	req.Header.Set("Authorization", "Bearer service-token")
	req.Header.Set("X-OJOS-Caller-Service", "judge-worker")
	rr := httptest.NewRecorder()
	handler(rr, req)

	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected service permission check to continue, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestAuthMiddlewareAuthorizesOnlyDelegatedAdminPermissionCheck(t *testing.T) {
	var gotService, gotToken, gotAPI, gotPermission string
	authorizer := func(
		_ context.Context,
		serviceCode string,
		credentialToken string,
		apiID string,
		permissionCode string,
	) (bool, error) {
		gotService = serviceCode
		gotToken = credentialToken
		gotAPI = apiID
		gotPermission = permissionCode
		return true, nil
	}
	middleware := NewAuthMiddleware("jwt-secret", "", authorizer)
	handler := middleware.Handle(func(w http.ResponseWriter, r *http.Request) {
		claims, ok := ClaimsFromContext(r.Context())
		if !ok || claims == nil || len(claims.Roles) != 1 || claims.Roles[0] != "internal" {
			t.Fatalf("expected delegated internal claims, got %#v", claims)
		}
		w.WriteHeader(http.StatusNoContent)
	})

	req := httptest.NewRequest(http.MethodPost, "/auth/admin/permission-check", nil)
	req.Header.Set("Authorization", "Bearer service-token")
	req.Header.Set("X-OJOS-Caller-Service", "user-service")
	req.Header.Set("X-OJOS-Api-Id", delegatedPermissionCheckAPI)
	rr := httptest.NewRecorder()
	handler(rr, req)

	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected delegated check to continue, got %d body=%s", rr.Code, rr.Body.String())
	}
	if gotService != "user-service" || gotToken != "service-token" ||
		gotAPI != delegatedPermissionCheckAPI || gotPermission != delegatedPermissionCheckPermission {
		t.Fatalf(
			"unexpected authorization input service=%q token=%q api=%q permission=%q",
			gotService,
			gotToken,
			gotAPI,
			gotPermission,
		)
	}
}

func TestAuthMiddlewareDoesNotReuseServiceCredentialOnOtherAdminRoutes(t *testing.T) {
	middleware := NewAuthMiddleware(
		"jwt-secret",
		"",
		func(context.Context, string, string, string, string) (bool, error) {
			return true, nil
		},
	)
	handler := middleware.Handle(func(w http.ResponseWriter, _ *http.Request) {
		t.Fatal("generic admin route must not accept a service credential")
	})

	req := httptest.NewRequest(http.MethodPost, "/auth/admin/roles", nil)
	req.Header.Set("Authorization", "Bearer service-token")
	req.Header.Set("X-OJOS-Caller-Service", "user-service")
	req.Header.Set("X-OJOS-Api-Id", delegatedPermissionCheckAPI)
	rr := httptest.NewRecorder()
	handler(rr, req)

	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestAuthMiddlewareRejectsDelegatedCheckWhenGrantIsDenied(t *testing.T) {
	middleware := NewAuthMiddleware(
		"jwt-secret",
		"",
		func(context.Context, string, string, string, string) (bool, error) {
			return false, nil
		},
	)
	handler := middleware.Handle(func(w http.ResponseWriter, _ *http.Request) {
		t.Fatal("denied service credential must not reach handler")
	})

	req := httptest.NewRequest(http.MethodPost, "/auth/admin/permission-check", nil)
	req.Header.Set("Authorization", "Bearer service-token")
	req.Header.Set("X-OJOS-Caller-Service", "user-service")
	req.Header.Set("X-OJOS-Api-Id", delegatedPermissionCheckAPI)
	rr := httptest.NewRecorder()
	handler(rr, req)

	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401, got %d body=%s", rr.Code, rr.Body.String())
	}
}
