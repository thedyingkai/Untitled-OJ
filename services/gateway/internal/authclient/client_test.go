package authclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHasSystemPermissionUsesUserPermissionEndpoint(t *testing.T) {
	var gotPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		if r.Header.Get("Authorization") == "" {
			t.Fatalf("authorization header must be forwarded to auth-service")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"allowed":true}}`))
	}))
	defer server.Close()

	allowed, err := New(server.URL).HasSystemPermission(context.Background(), "Bearer token", 42, "demo.read")
	if err != nil {
		t.Fatalf("permission check failed: %v", err)
	}
	if !allowed {
		t.Fatalf("expected permission to be allowed")
	}
	if gotPath != "/auth/permission-check" {
		t.Fatalf("gateway must not use auth admin permission endpoint, got %q", gotPath)
	}
}
