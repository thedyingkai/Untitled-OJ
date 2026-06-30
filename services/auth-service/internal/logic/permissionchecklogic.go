// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type PermissionCheckLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewPermissionCheckLogic(ctx context.Context, svcCtx *svc.ServiceContext) *PermissionCheckLogic {
	return &PermissionCheckLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *PermissionCheckLogic) PermissionCheck(req *types.PermissionCheckReq) (resp *types.PermissionCheckResp, err error) {
	return NewAdminPermissionsLogic(l.ctx, l.svcCtx).CheckPermission(req)
}
