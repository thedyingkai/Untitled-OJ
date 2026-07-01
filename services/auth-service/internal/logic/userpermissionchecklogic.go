package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type UserPermissionCheckLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUserPermissionCheckLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UserPermissionCheckLogic {
	return &UserPermissionCheckLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UserPermissionCheckLogic) UserPermissionCheck(req *types.PermissionCheckReq) (*types.PermissionCheckResp, error) {
	claims, ok := middleware.ClaimsFromContext(l.ctx)
	if !ok || claims == nil {
		return nil, errors.New("unauthorized")
	}
	if strings.TrimSpace(req.Permission) == "" {
		return nil, errors.New("permission is required")
	}
	scopeType := strings.TrimSpace(req.ScopeType)
	if scopeType == "" {
		scopeType = "system"
	}

	callerType := strings.ToLower(strings.TrimSpace(req.CallerType))
	if callerType == "" {
		callerType = "user"
	}
	if callerType == "service" || callerType == "internal" {
		callerService := strings.TrimSpace(req.CallerService)
		if callerService == "" {
			return nil, errors.New("caller_service is required")
		}
		allowed, err := l.svcCtx.AdminRepo.ServiceCallerCanUsePermission(
			l.ctx,
			callerService,
			strings.TrimSpace(req.Permission),
		)
		if err != nil {
			return nil, err
		}
		return &types.PermissionCheckResp{
			Code: 0,
			Msg:  "success",
			Data: types.PermissionCheckData{Allowed: allowed},
		}, nil
	}

	if claims.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	if req.UserId != 0 && req.UserId != claims.UserID {
		return nil, errors.New("cannot check another user's permission")
	}
	allowed, err := permission.HasUserPermission(
		l.ctx,
		l.svcCtx.DB,
		claims.UserID,
		strings.TrimSpace(req.Permission),
		permission.Scope{Type: scopeType, ID: req.ScopeId},
	)
	if err != nil {
		return nil, err
	}
	return &types.PermissionCheckResp{
		Code: 0,
		Msg:  "success",
		Data: types.PermissionCheckData{Allowed: allowed},
	}, nil
}
