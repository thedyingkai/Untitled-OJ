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
	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return errors.New("permission checker is not configured")
	}
	// Mirror the operation-level OpenAPI/Gateway permission even for direct
	// provider calls, then apply the row-level escalation for another user's
	// submission.
	if err := checker.RequireUserPermission(
		ctx,
		user.UserID,
		"judge.submission.view.own",
		sharedperm.SystemScope(),
	); err != nil {
		return err
	}
	if submission.UserID == user.UserID {
		return nil
	}

	ok, err := checker.HasUserPermission(
		ctx,
		user.UserID,
		"judge.submission.view.all",
		sharedperm.SystemScope(),
	)
	if err != nil {
		return err
	}
	if ok {
		return nil
	}

	return sharedperm.ErrForbidden
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
		"judge.submission.view.all",
		sharedperm.SystemScope(),
	)
	if err != nil {
		return err
	}
	if ok {
		return nil
	}

	return sharedperm.ErrForbidden
}
