package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/svc"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"
)

func requireProblemDataPermission(ctx context.Context, svcCtx *svc.ServiceContext, problemID int64) error {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return errors.New("unauthorized")
	}
	if problemID <= 0 {
		return errors.New("invalid problem id")
	}

	return permission.RequireUserPermission(
		ctx,
		svcCtx.DB,
		user.UserID,
		"problem.manage.data",
		permission.Scope{Type: "problem", ID: problemID},
	)
}
