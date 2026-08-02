package authclient

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHasSystemPermissionUsesUserPermissionEndpoint(t *testing.T) {
	var gotPath string
	var gotCallerService string
	var payload permissionCheckRequest
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotCallerService = r.Header.Get("X-OJOS-Caller-Service")
		if r.Header.Get("Authorization") == "" {
			t.Fatalf("authorization header must be forwarded to auth-service")
		}
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			t.Fatalf("decode request: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"allowed":true}}`))
	}))
	defer server.Close()

	allowed, err := New(server.URL).HasSystemPermission(context.Background(), "Bearer token", PermissionCaller{
		Type:   "user",
		UserID: 42,
		APIID:  "demo.read",
	}, "demo.read")
	if err != nil {
		t.Fatalf("permission check failed: %v", err)
	}
	if !allowed {
		t.Fatalf("expected permission to be allowed")
	}
	if gotPath != "/auth/permission-check" {
		t.Fatalf("gateway must not use auth admin permission endpoint, got %q", gotPath)
	}
	if gotCallerService != "" {
		t.Fatalf("user permission request must not claim a service identity, got %q", gotCallerService)
	}
	if payload.CallerType != "user" || payload.UserID != 42 || payload.Permission != "demo.read" || payload.APIID != "demo.read" {
		t.Fatalf("unexpected permission payload: %#v", payload)
	}
}

func TestHasSystemPermissionBindsServiceCredentialToCallerHeader(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("X-OJOS-Caller-Service"); got != "judge-worker" {
			t.Fatalf("service credential must be bound to caller header, got %q", got)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer service-token" {
			t.Fatalf("unexpected authorization header %q", got)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"allowed":true}}`))
	}))
	defer server.Close()

	allowed, err := New(server.URL).HasSystemPermission(context.Background(), "Bearer service-token", PermissionCaller{
		Type:    "service",
		Service: "judge-worker",
		APIID:   "storage.object.get",
	}, "storage.object.read")
	if err != nil {
		t.Fatalf("permission check failed: %v", err)
	}
	if !allowed {
		t.Fatal("expected permission to be allowed")
	}
}
