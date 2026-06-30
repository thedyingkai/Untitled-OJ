package logic

import (
	"context"
	"errors"

	"ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
	"ojos-shared/security/permission"
)

func requireAdmin(ctx context.Context, svcCtx *svc.ServiceContext) (int64, error) {
	claims, ok := middleware.ClaimsFromContext(ctx)
	if !ok || claims == nil || claims.UserID <= 0 {
		return 0, errors.New("unauthorized")
	}
	for _, role := range claims.Roles {
		if role == "super_admin" || role == "admin" {
			return claims.UserID, nil
		}
	}
	if err := permission.RequireUserPermission(
		ctx,
		svcCtx.DB,
		claims.UserID,
		"system.admin",
		permission.SystemScope(),
	); err != nil {
		return 0, err
	}
	return claims.UserID, nil
}
