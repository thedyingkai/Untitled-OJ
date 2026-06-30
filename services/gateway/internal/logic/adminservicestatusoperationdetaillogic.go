// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceStatusOperationDetailLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceStatusOperationDetailLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceStatusOperationDetailLogic {
	return &AdminServiceStatusOperationDetailLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceStatusOperationDetailLogic) AdminServiceStatusOperationDetail(req *types.AdminPathIdReq) (resp *types.ServiceStatusOperationsResp, err error) {
	return NewAdminServiceStatusLogic(l.ctx, l.svcCtx).OperationDetail(req.Authorization, req.Id)
}
