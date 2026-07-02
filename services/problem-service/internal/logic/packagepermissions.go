package logic

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"ojos-problem-service/internal/svc"
	"ojos-shared/security/authctx"
)

func requireProblemDataPermission(ctx context.Context, svcCtx *svc.ServiceContext, problemID int64) error {
	_, err := requireProblemPermission(ctx, svcCtx, "problem.manage.data", problemID)
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
	if userHasProblemPermission(user, permission) {
		return user, nil
	}
	return nil, fmt.Errorf("forbidden: missing %s", permission)
}

func requireSystemProblemPermission(ctx context.Context, svcCtx *svc.ServiceContext, permission string) (*authctx.UserContext, error) {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	if userHasProblemPermission(user, permission) {
		return user, nil
	}
	return nil, fmt.Errorf("forbidden: missing %s", permission)
}

func userCanViewPrivateProblems(user *authctx.UserContext) bool {
	for role := range normalizedRoles(user) {
		switch role {
		case "super_admin", "admin", "problem_setter", "problem_owner", "problem_data_manager":
			return true
		}
	}
	return false
}

func userHasProblemPermission(user *authctx.UserContext, permission string) bool {
	roles := normalizedRoles(user)
	if hasRole(roles, "super_admin") || hasRole(roles, "admin") {
		return true
	}

	switch strings.TrimSpace(permission) {
	case "problem.view":
		return hasAnyRole(roles, "user", "problem_viewer", "problem_setter", "problem_owner", "problem_data_manager")
	case "problem.create":
		return hasAnyRole(roles, "problem_setter", "problem_owner")
	case "problem.edit", "problem.delete":
		return hasAnyRole(roles, "problem_setter", "problem_owner")
	case "problem.manage.data", "problem.testdata.read", "problem.testdata.write":
		return hasAnyRole(roles, "problem_setter", "problem_owner", "problem_data_manager")
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
