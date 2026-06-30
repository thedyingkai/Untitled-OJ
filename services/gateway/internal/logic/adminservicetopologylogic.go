// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceTopologyLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceTopologyLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceTopologyLogic {
	return &AdminServiceTopologyLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceTopologyLogic) AdminServiceTopology(req *types.AdminAuthReq) (resp *types.ServiceTopologyResp, err error) {
	return NewAdminServicesLogic(l.ctx, l.svcCtx).Topology(req.Authorization)
}
