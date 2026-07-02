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
