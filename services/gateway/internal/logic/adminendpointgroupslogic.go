// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminEndpointGroupsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminEndpointGroupsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminEndpointGroupsLogic {
	return &AdminEndpointGroupsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminEndpointGroupsLogic) AdminEndpointGroups(req *types.AdminAuthReq) (resp *types.ListEndpointGroupsResp, err error) {
	return NewAdminServicesLogic(l.ctx, l.svcCtx).ListEndpointGroups(req.Authorization)
}
