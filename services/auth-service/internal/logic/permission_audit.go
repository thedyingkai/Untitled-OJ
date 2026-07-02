package logic

import (
	"context"
	"strings"

	"ojos-auth-service/internal/svc"
	"ojos-shared/security/permission"
)

func auditUserPermissionCheck(ctx context.Context, svcCtx *svc.ServiceContext, actorID int64, targetUserID int64, permissionCode string, scope permission.Scope, allowed bool, callerType string, apiID string) error {
	if svcCtx == nil || svcCtx.DB == nil {
		return nil
	}
	action := "user.permission_check.deny"
	if allowed {
		action = "user.permission_check.allow"
	}
	if strings.TrimSpace(callerType) == "admin" {
		action = "admin.permission_check.deny"
		if allowed {
			action = "admin.permission_check.allow"
		}
	}
	return permission.WriteAuditLog(
		ctx,
		svcCtx.DB,
		permission.UserPrincipal(actorID),
		action,
		permission.UserPrincipal(targetUserID),
		strings.TrimSpace(permissionCode),
		0,
		"",
		scope,
		"",
		map[string]any{
			"allowed":     allowed,
			"caller_type": strings.TrimSpace(callerType),
			"api_id":      strings.TrimSpace(apiID),
		},
	)
}
