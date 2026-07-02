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

	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return errors.New("permission checker is not configured")
	}

	ok, err := checker.HasUserPermission(
		ctx,
		user.UserID,
		"submission.view.all",
		sharedperm.SystemScope(),
	)
	if err != nil {
		return err
	}
	if ok {
		return nil
	}

	return checker.RequireUserPermission(
		ctx,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: submission.ProblemID},
	)
}

func requireSubmissionDebugPermission(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	submission *repository.SubmissionView,
) error {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return errors.New("unauthorized")
	}

	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return errors.New("permission checker is not configured")
	}

	ok, err := checker.HasUserPermission(
		ctx,
		user.UserID,
		"submission.view.all",
		sharedperm.SystemScope(),
	)
	if err != nil {
		return err
	}
	if ok {
		return nil
	}

	return checker.RequireUserPermission(
		ctx,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: submission.ProblemID},
	)
}
