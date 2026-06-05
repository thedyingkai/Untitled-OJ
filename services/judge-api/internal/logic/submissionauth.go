package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func requireSubmissionViewPermission(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	submission *repository.SubmissionView,
) error {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return errors.New("unauthorized")
	}

	if submission.UserID == user.UserID {
		return nil
	}

	return sharedperm.RequireUserPermission(
		ctx,
		svcCtx.DB,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: submission.ProblemID},
	)
}
