package logic

import (
	"context"
	"errors"
	"testing"

	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
	"ojos-user-service/internal/svc"
)

type fakeUserChecker struct {
	allowed map[string]bool
}

func (f fakeUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	ok, err := f.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !ok {
		return sharedperm.ErrForbidden
	}
	return nil
}

func (f fakeUserChecker) HasUserPermission(_ context.Context, _ int64, permissionCode string, _ sharedperm.Scope) (bool, error) {
	return f.allowed[permissionCode], nil
}

func TestRequireUserProfilePermissionAllowsSelfPermission(t *testing.T) {
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 42, Username: "alice"})
	svcCtx := &svc.ServiceContext{
		Permission: fakeUserChecker{allowed: map[string]bool{
			"user.profile.read.self": true,
		}},
	}

	err := requireUserProfilePermission(ctx, svcCtx, "42", "user.profile.read.self", "user.profile.read.any")
	if err != nil {
		t.Fatalf("self read should be allowed: %v", err)
	}
}

func TestRequireUserProfilePermissionRequiresAnyPermissionForOtherUser(t *testing.T) {
	ctx := authctx.NewContext(context.Background(), &authctx.UserContext{UserID: 42, Username: "alice"})
	svcCtx := &svc.ServiceContext{
		Permission: fakeUserChecker{allowed: map[string]bool{
			"user.profile.read.self": true,
		}},
	}

	err := requireUserProfilePermission(ctx, svcCtx, "99", "user.profile.read.self", "user.profile.read.any")
	if !errors.Is(err, sharedperm.ErrForbidden) {
		t.Fatalf("other user read should require any permission, got %v", err)
	}
}
