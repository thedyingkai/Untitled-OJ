// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListPermissionsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListPermissionsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListPermissionsLogic {
	return &ListPermissionsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListPermissionsLogic) ListPermissions(req *types.ListQueryReq) (resp *types.ListPermissionsResp, err error) {
	return NewAdminPermissionsLogic(l.ctx, l.svcCtx).ListPermissions(req)
}
