package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/svc"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func requireJudgeAdmin(ctx context.Context, svcCtx *svc.ServiceContext) error {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return errors.New("unauthorized")
	}
	for _, role := range user.Roles {
		if role == "super_admin" || role == "admin" {
			return nil
		}
	}
	return sharedperm.RequireUserPermission(
		ctx,
		svcCtx.DB,
		user.UserID,
		"system.admin",
		sharedperm.SystemScope(),
	)
}
