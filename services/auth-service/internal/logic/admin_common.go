package logic

import (
	"context"

	"ojos-auth-service/internal/apperror"
	"ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
	"ojos-shared/security/permission"
)

func requireAdmin(ctx context.Context, svcCtx *svc.ServiceContext) (int64, error) {
	claims, ok := middleware.ClaimsFromContext(ctx)
	if !ok || claims == nil || claims.UserID <= 0 {
		if claims != nil {
			for _, role := range claims.Roles {
				if role == "internal" {
					return 1, nil
				}
			}
		}
		return 0, apperror.Unauthorized(apperror.CodeUnauthorized, "admin authentication required")
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
		return 0, apperror.Forbidden(apperror.CodeAdminRequired, "system.admin permission required")
	}
	return claims.UserID, nil
}
