package logic

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"ojos-problem-service/internal/svc"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func requireProblemDataPermission(ctx context.Context, svcCtx *svc.ServiceContext, problemID int64) error {
	_, err := requireProblemPermission(ctx, svcCtx, "problem.testdata.read", problemID)
	return err
}

func requireProblemPermission(ctx context.Context, svcCtx *svc.ServiceContext, permission string, problemID int64) (*authctx.UserContext, error) {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	if problemID <= 0 {
		return nil, errors.New("invalid problem id")
	}
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return nil, errors.New("permission checker is not configured")
	}
	allowed, err := checker.HasUserPermission(
		ctx,
		user.UserID,
		permission,
		sharedperm.Scope{Type: "problem", ID: problemID},
	)
	if err != nil {
		return nil, err
	}
	if allowed {
		return user, nil
	}
	if isOwnerScopedProblemPermission(permission) && svcCtx != nil && svcCtx.Repo != nil {
		owner, err := svcCtx.Repo.IsProblemOwner(ctx, user.UserID, problemID)
		if err != nil {
			return nil, err
		}
		if owner {
			return user, nil
		}
	}
	if hasRole(normalizedRoles(user), "super_admin") {
		return user, nil
	}
	return nil, fmt.Errorf("forbidden: missing %s", permission)
}

func requireSystemProblemPermission(ctx context.Context, svcCtx *svc.ServiceContext, permission string) (*authctx.UserContext, error) {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return nil, errors.New("permission checker is not configured")
	}
	allowed, err := checker.HasUserPermission(
		ctx,
		user.UserID,
		permission,
		sharedperm.SystemScope(),
	)
	if err != nil {
		return nil, err
	}
	if allowed || hasRole(normalizedRoles(user), "super_admin") {
		return user, nil
	}
	return nil, fmt.Errorf("forbidden: missing %s", permission)
}

func userCanViewPrivateProblems(ctx context.Context, svcCtx *svc.ServiceContext, user *authctx.UserContext) (bool, error) {
	if user == nil || user.UserID <= 0 {
		return false, nil
	}
	roles := normalizedRoles(user)
	if hasRole(roles, "super_admin") {
		return true, nil
	}
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return false, errors.New("permission checker is not configured")
	}
	return checker.HasUserPermission(
		ctx,
		user.UserID,
		"problem.view.private",
		sharedperm.SystemScope(),
	)
}

func userHasProblemPermission(ctx context.Context, svcCtx *svc.ServiceContext, user *authctx.UserContext, permission string) (bool, error) {
	if user == nil || user.UserID <= 0 {
		return false, nil
	}
	if hasRole(normalizedRoles(user), "super_admin") {
		return true, nil
	}
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return false, errors.New("permission checker is not configured")
	}
	return checker.HasUserPermission(ctx, user.UserID, permission, sharedperm.SystemScope())
}

func isOwnerScopedProblemPermission(permission string) bool {
	switch strings.TrimSpace(permission) {
	case "problem.view", "problem.view.private", "problem.edit", "problem.delete", "problem.manage.data", "problem.testdata.read", "problem.testdata.write":
		return true
	default:
		return false
	}
}

func normalizedRoles(user *authctx.UserContext) map[string]bool {
	roles := map[string]bool{}
	if user == nil {
		return roles
	}
	for _, role := range user.Roles {
		role = strings.ToLower(strings.TrimSpace(role))
		if role != "" {
			roles[role] = true
		}
	}
	return roles
}

func hasAnyRole(roles map[string]bool, names ...string) bool {
	for _, name := range names {
		if hasRole(roles, name) {
			return true
		}
	}
	return false
}

func hasRole(roles map[string]bool, name string) bool {
	return roles[strings.ToLower(strings.TrimSpace(name))]
}
