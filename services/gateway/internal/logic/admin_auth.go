package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-gateway/internal/svc"
	sharedjwt "ojos-shared/security/jwt"
)

func requireAdmin(ctx context.Context, svcCtx *svc.ServiceContext, authHeader string) error {
	_, err := requireAdminClaims(ctx, svcCtx, authHeader)
	return err
}

type adminClaims struct {
	UserID   int64
	Username string
	Roles    []string
}

func requireAdminClaims(ctx context.Context, svcCtx *svc.ServiceContext, authHeader string) (adminClaims, error) {
	claims, err := parseBearerClaims(svcCtx, authHeader)
	if err != nil {
		return adminClaims{}, err
	}
	if isAdminRole(claims.Roles) {
		return adminClaims{UserID: claims.UserID, Username: claims.Username, Roles: claims.Roles}, nil
	}
	ok, err := hasSystemAdminPermission(ctx, svcCtx, authHeader, claims.UserID)
	if err != nil {
		return adminClaims{}, err
	}
	if !ok {
		return adminClaims{}, errors.New("forbidden")
	}
	return adminClaims{UserID: claims.UserID, Username: claims.Username, Roles: claims.Roles}, nil
}

var hasSystemAdminPermission = func(ctx context.Context, svcCtx *svc.ServiceContext, authHeader string, userID int64) (bool, error) {
	if svcCtx == nil || svcCtx.AuthClient == nil {
		return false, errors.New("auth-service permission client is not configured")
	}
	return svcCtx.AuthClient.HasSystemPermission(ctx, authHeader, userID, "system.admin")
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
