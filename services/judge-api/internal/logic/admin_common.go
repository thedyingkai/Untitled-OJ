package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/svc"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func requireJudgePermission(ctx context.Context, svcCtx *svc.ServiceContext, permission string) error {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return errors.New("unauthorized")
	}
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return errors.New("permission checker is not configured")
	}
	return checker.RequireUserPermission(
		ctx,
		user.UserID,
		permission,
		sharedperm.SystemScope(),
	)
}

func requireJudgeAdmin(ctx context.Context, svcCtx *svc.ServiceContext) error {
	return requireJudgePermission(ctx, svcCtx, "judge.admin")
}
