package permission

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestAuthServiceUserCheckerUsesAdminPermissionCheck(t *testing.T) {
	var gotAuth string
	var gotPath string
	var gotUserID int64
	var gotPermission string
	var gotScopeType string
	var gotScopeID int64

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		gotPath = r.URL.Path

		var payload permissionCheckRequest
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			t.Fatalf("decode permission request: %v", err)
		}
		gotUserID = payload.UserID
		gotPermission = payload.Permission
		gotScopeType = payload.ScopeType
		gotScopeID = payload.ScopeID

		_ = json.NewEncoder(w).Encode(permissionCheckResponse{
			Code: 0,
			Msg:  "success",
			Data: struct {
				Allowed bool `json:"allowed"`
			}{Allowed: true},
		})
	}))
	defer server.Close()

	checker := NewAuthServiceUserChecker(server.URL, "internal-token")
	if checker == nil {
		t.Fatal("expected auth-service permission checker")
	}

	allowed, err := checker.HasUserPermission(t.Context(), 42, "judge.submit", Scope{Type: "problem", ID: 1001})
	if err != nil {
		t.Fatalf("permission check failed: %v", err)
	}
	if !allowed {
		t.Fatal("expected permission allowed")
	}
	if gotPath != "/auth/admin/permission-check" {
		t.Fatalf("unexpected permission endpoint %q", gotPath)
	}
	if gotAuth != "Bearer internal-token" {
		t.Fatalf("unexpected authorization header %q", gotAuth)
	}
	if gotUserID != 42 || gotPermission != "judge.submit" || gotScopeType != "problem" || gotScopeID != 1001 {
		t.Fatalf("unexpected permission payload user=%d permission=%q scope=%s/%d", gotUserID, gotPermission, gotScopeType, gotScopeID)
	}
}

func TestAuthServiceUserCheckerRequireMapsDeniedToForbidden(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(permissionCheckResponse{
			Code: 0,
			Msg:  "success",
			Data: struct {
				Allowed bool `json:"allowed"`
			}{Allowed: false},
		})
	}))
	defer server.Close()

	checker := NewAuthServiceUserChecker(server.URL, "internal-token")
	if checker == nil {
		t.Fatal("expected auth-service permission checker")
	}

	err := checker.RequireUserPermission(t.Context(), 42, "judge.admin", SystemScope())
	if !errors.Is(err, ErrForbidden) {
		t.Fatalf("expected forbidden, got %v", err)
	}
}

func TestRemoteUserCheckerPrefersInternalGatewayRoute(t *testing.T) {
	var gotPath string
	var gotAuth string
	var gotCallerService string
	var gotNodeID string
	var gotCallerNodeID string
	var payload permissionCheckRequest

	gateway := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotAuth = r.Header.Get("Authorization")
		gotCallerService = r.Header.Get("X-OJOS-Caller-Service")
		gotNodeID = r.Header.Get("X-OJOS-Node-Id")
		gotCallerNodeID = r.Header.Get("X-OJOS-Caller-Node-Id")

		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			t.Fatalf("decode permission request: %v", err)
		}

		_ = json.NewEncoder(w).Encode(permissionCheckResponse{
			Code: 0,
			Msg:  "success",
			Data: struct {
				Allowed bool `json:"allowed"`
			}{Allowed: true},
		})
	}))
	defer gateway.Close()

	checker := NewRemoteUserChecker(RemoteCheckerConfig{
		InternalGatewayEndpoint: gateway.URL,
		CallerService:           "user-service",
		CallerNodeID:            "child-node",
		ServiceToken:            "service-token",
		// Configured but must not win: the gateway route is preferred.
		AuthServiceEndpoint:   "http://auth-service:8081",
		AuthServiceAdminToken: "internal-token",
	})
	if checker == nil {
		t.Fatal("expected internal gateway permission checker")
	}
	routed, ok := checker.(RemoteUserChecker)
	if !ok || routed.Route() != RouteInternalGateway {
		t.Fatalf("expected route %q, got %#v", RouteInternalGateway, checker)
	}

	allowed, err := checker.HasUserPermission(t.Context(), 42, "judge.submit", Scope{Type: "problem", ID: 1001})
	if err != nil {
		t.Fatalf("permission check failed: %v", err)
	}
	if !allowed {
		t.Fatal("expected permission allowed")
	}
	if gotPath != "/internal/apis/"+DefaultPermissionCheckApiID {
		t.Fatalf("unexpected internal api path %q", gotPath)
	}
	if gotAuth != "Bearer service-token" {
		t.Fatalf("unexpected authorization header %q", gotAuth)
	}
	if gotCallerService != "user-service" {
		t.Fatalf("unexpected caller service header %q", gotCallerService)
	}
	if gotNodeID != "child-node" || gotCallerNodeID != "child-node" {
		t.Fatalf("unexpected node headers node=%q caller_node=%q", gotNodeID, gotCallerNodeID)
	}
	if payload.UserID != 42 ||
		payload.Permission != "judge.submit" ||
		payload.ScopeType != "problem" ||
		payload.ScopeID != 1001 ||
		payload.CallerService != "user-service" ||
		payload.CallerNodeID != "child-node" ||
		payload.ApiID != DefaultPermissionCheckApiID {
		t.Fatalf("unexpected permission payload %#v", payload)
	}
}

func TestRemoteUserCheckerFallsBackWhenGatewayIsNotConfigured(t *testing.T) {
	checker := NewRemoteUserChecker(RemoteCheckerConfig{
		AuthServiceEndpoint:   "http://auth-service:8081",
		AuthServiceAdminToken: "internal-token",
	})
	routed, ok := checker.(RemoteUserChecker)
	if !ok || routed.Route() != RouteAuthService {
		t.Fatalf("expected route %q, got %#v", RouteAuthService, checker)
	}

	// An incomplete gateway route must not silently disable permission checks.
	incomplete := NewRemoteUserChecker(RemoteCheckerConfig{
		InternalGatewayEndpoint: "http://gateway:8080",
		AuthServiceEndpoint:     "http://auth-service:8081",
		AuthServiceAdminToken:   "internal-token",
	})
	routed, ok = incomplete.(RemoteUserChecker)
	if !ok || routed.Route() != RouteAuthService {
		t.Fatalf("expected fallback to %q, got %#v", RouteAuthService, incomplete)
	}

	// The auth-service admin token is not a service credential. Even with a
	// caller identity present, it must never be reused on the gateway route.
	noServiceCredential := NewRemoteUserChecker(RemoteCheckerConfig{
		InternalGatewayEndpoint: "http://gateway:8080",
		CallerService:           "user-service",
		AuthServiceEndpoint:     "http://auth-service:8081",
		AuthServiceAdminToken:   "internal-token",
	})
	routed, ok = noServiceCredential.(RemoteUserChecker)
	if !ok || routed.Route() != RouteAuthService {
		t.Fatalf(
			"expected missing service credential to fall back to %q, got %#v",
			RouteAuthService,
			noServiceCredential,
		)
	}

	if NewRemoteUserChecker(RemoteCheckerConfig{}) != nil {
		t.Fatal("expected nil checker when no remote route is configured")
	}
}
