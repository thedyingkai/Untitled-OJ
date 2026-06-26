package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-gateway/internal/svc"
	sharedjwt "ojos-shared/security/jwt"
	sharedperm "ojos-shared/security/permission"
)

func requireAdmin(ctx context.Context, svcCtx *svc.ServiceContext, authHeader string) error {
	claims, err := parseBearerClaims(svcCtx, authHeader)
	if err != nil {
		return err
	}
	if isAdminRole(claims.Roles) {
		return nil
	}
	ok, err := hasSystemAdminPermission(ctx, svcCtx, claims.UserID)
	if err != nil {
		return err
	}
	if !ok {
		return errors.New("forbidden")
	}
	return nil
}

var hasSystemAdminPermission = func(ctx context.Context, svcCtx *svc.ServiceContext, userID int64) (bool, error) {
	return sharedperm.HasUserPermission(
		ctx,
		svcCtx.DB,
		userID,
		"system.admin",
		sharedperm.SystemScope(),
	)
}

func parseBearerClaims(svcCtx *svc.ServiceContext, authHeader string) (*sharedjwt.Claims, error) {
	parts := strings.Fields(strings.TrimSpace(authHeader))
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		return nil, errors.New("missing authorization header")
	}
	return sharedjwt.Parse(svcCtx.Config.Jwt.Secret, parts[1])
}

func isAdminRole(roles []string) bool {
	for _, role := range roles {
		if role == "super_admin" || role == "admin" {
			return true
		}
	}
	return false
}
