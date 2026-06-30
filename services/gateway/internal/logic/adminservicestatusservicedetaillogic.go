// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceStatusServiceDetailLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceStatusServiceDetailLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceStatusServiceDetailLogic {
	return &AdminServiceStatusServiceDetailLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceStatusServiceDetailLogic) AdminServiceStatusServiceDetail(req *types.AdminPathIdReq) (resp *types.ServiceStatusItemResp, err error) {
	return NewAdminServiceStatusLogic(l.ctx, l.svcCtx).ServiceDetail(req.Authorization, req.Id)
}
