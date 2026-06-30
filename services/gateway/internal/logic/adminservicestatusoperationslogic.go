// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceStatusOperationsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceStatusOperationsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceStatusOperationsLogic {
	return &AdminServiceStatusOperationsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceStatusOperationsLogic) AdminServiceStatusOperations(req *types.AdminAuthReq) (resp *types.ServiceStatusOperationsResp, err error) {
	return NewAdminServiceStatusLogic(l.ctx, l.svcCtx).Operations(req.Authorization)
}
