// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceDetailLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceDetailLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceDetailLogic {
	return &AdminServiceDetailLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceDetailLogic) AdminServiceDetail(req *types.AdminPathIdReq) (resp *types.ServiceDetailResp, err error) {
	return NewAdminServicesLogic(l.ctx, l.svcCtx).Detail(req.Authorization, req.Id)
}
