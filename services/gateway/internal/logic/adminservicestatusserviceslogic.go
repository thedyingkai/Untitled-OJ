// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceStatusServicesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminServiceStatusServicesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceStatusServicesLogic {
	return &AdminServiceStatusServicesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminServiceStatusServicesLogic) AdminServiceStatusServices(req *types.AdminAuthReq) (resp *types.ServiceStatusListResp, err error) {
	return NewAdminServiceStatusLogic(l.ctx, l.svcCtx).ListServices(req.Authorization)
}
