package logic

import (
	"context"
	"errors"
	"strconv"
	"strings"

	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
	"ojos-user-service/internal/svc"
)

func currentUser(ctx context.Context) (*authctx.UserContext, error) {
	user, ok := authctx.FromContext(ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	return user, nil
}

func currentUserIDString(ctx context.Context) (string, string, error) {
	user, err := currentUser(ctx)
	if err != nil {
		return "", "", err
	}
	return strconv.FormatInt(user.UserID, 10), strings.TrimSpace(user.Username), nil
}

func requireUserProfilePermission(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	targetUserID string,
	selfPermission string,
	otherPermission string,
) error {
	user, err := currentUser(ctx)
	if err != nil {
		return err
	}
	targetUserID = strings.TrimSpace(targetUserID)
	if targetUserID == "" {
		return errors.New("invalid user id")
	}

	permissionCode := otherPermission
	if targetUserID == strconv.FormatInt(user.UserID, 10) {
		permissionCode = selfPermission
	}
	if strings.TrimSpace(permissionCode) == "" {
		return errors.New("permission is not configured")
	}

	checker := svcCtx.ActivePermissionChecker()
	if checker == nil {
		return errors.New("permission checker is not configured")
	}
	return checker.RequireUserPermission(ctx, user.UserID, permissionCode, sharedperm.SystemScope())
}
