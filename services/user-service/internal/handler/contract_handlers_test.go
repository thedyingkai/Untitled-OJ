package handler

import (
	"context"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
	"ojos-user-service/internal/store"
	"ojos-user-service/internal/svc"

	"github.com/zeromicro/go-zero/rest/pathvar"
)

type recordingChecker struct {
	allowed map[string]bool
	calls   []string
	err     error
}

func (checker *recordingChecker) RequireUserPermission(ctx context.Context, userID int64, permission string, scope sharedperm.Scope) error {
	allowed, err := checker.HasUserPermission(ctx, userID, permission, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return sharedperm.ErrForbidden
	}
	return nil
}

func (checker *recordingChecker) HasUserPermission(_ context.Context, _ int64, permission string, _ sharedperm.Scope) (bool, error) {
	checker.calls = append(checker.calls, permission)
	if checker.err != nil {
		return false, checker.err
	}
	return checker.allowed[permission], nil
}

func contractServiceContext(t *testing.T, checker sharedperm.UserChecker) *svc.ServiceContext {
	t.Helper()
	profiles, err := store.NewFileProfileStore(filepath.Join(t.TempDir(), "profiles"))
	if err != nil {
		t.Fatal(err)
	}
	return &svc.ServiceContext{ProfileStore: profiles, Permission: checker}
}

func authenticatedRequest(method, target, body string) *http.Request {
	request := httptest.NewRequest(method, target, strings.NewReader(body))
	request.Header.Set("Content-Type", "application/json")
	user := &authctx.UserContext{UserID: 42, Username: "alice"}
	return request.WithContext(authctx.NewContext(request.Context(), user))
}

func TestAdminProfileRouteUsesOnlyDeclaredAnyPermission(t *testing.T) {
	checker := &recordingChecker{allowed: map[string]bool{"user.profile.read.any": true}}
	ctx := contractServiceContext(t, checker)
	handler := requireOperationPermission(ctx, "user.profile.read.any", adminGetProfileHandler(ctx))
	request := authenticatedRequest(http.MethodGet, "/admin/users/42/profile", "")
	request = pathvar.WithVars(request, map[string]string{"user_id": "42"})
	response := httptest.NewRecorder()
	handler(response, request)
	if response.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", response.Code, response.Body.String())
	}
	if len(checker.calls) != 1 || checker.calls[0] != "user.profile.read.any" {
		t.Fatalf("permission calls = %v", checker.calls)
	}
}

func TestCurrentProfileRouteUsesOnlyDeclaredSelfPermission(t *testing.T) {
	checker := &recordingChecker{allowed: map[string]bool{"user.profile.read.self": true}}
	ctx := contractServiceContext(t, checker)
	handler := requireOperationPermission(ctx, "user.profile.read.self", getMyProfileHandler(ctx))
	response := httptest.NewRecorder()
	handler(response, authenticatedRequest(http.MethodGet, "/api/users/me", ""))
	if response.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", response.Code, response.Body.String())
	}
	if len(checker.calls) != 1 || checker.calls[0] != "user.profile.read.self" {
		t.Fatalf("permission calls = %v", checker.calls)
	}
}

func TestPermissionDependencyFailureIsFailClosedAsUnavailable(t *testing.T) {
	checker := &recordingChecker{err: context.DeadlineExceeded}
	ctx := contractServiceContext(t, checker)
	handler := requireOperationPermission(ctx, "user.profile.read.self", getMyProfileHandler(ctx))
	response := httptest.NewRecorder()
	handler(response, authenticatedRequest(http.MethodGet, "/api/users/me", ""))
	if response.Code != http.StatusServiceUnavailable {
		t.Fatalf("status = %d, body = %s", response.Code, response.Body.String())
	}
}

func TestCurrentProfileUpdateRejectsTargetUserSmuggling(t *testing.T) {
	checker := &recordingChecker{allowed: map[string]bool{"user.profile.update.self": true}}
	ctx := contractServiceContext(t, checker)
	handler := requireOperationPermission(ctx, "user.profile.update.self", updateMeHandler(ctx))
	response := httptest.NewRecorder()
	handler(response, authenticatedRequest(http.MethodPatch, "/api/users/me", `{"user_id":"99","display_name":"Alice"}`))
	if response.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, body = %s", response.Code, response.Body.String())
	}
}

func TestCurrentProfileUpdateBindsToAuthenticatedUser(t *testing.T) {
	checker := &recordingChecker{allowed: map[string]bool{"user.profile.update.self": true}}
	ctx := contractServiceContext(t, checker)
	handler := requireOperationPermission(ctx, "user.profile.update.self", updateMeHandler(ctx))
	response := httptest.NewRecorder()
	handler(response, authenticatedRequest(http.MethodPatch, "/api/users/me", `{"display_name":"Alice"}`))
	if response.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", response.Code, response.Body.String())
	}
	if !strings.Contains(response.Body.String(), `"user_id":"42"`) {
		t.Fatalf("response did not bind update to current user: %s", response.Body.String())
	}
}
