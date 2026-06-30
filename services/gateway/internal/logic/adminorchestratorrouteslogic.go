// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminOrchestratorRoutesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminOrchestratorRoutesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminOrchestratorRoutesLogic {
	return &AdminOrchestratorRoutesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminOrchestratorRoutesLogic) AdminOrchestratorRoutes(req *types.AdminRoutesReq) (resp *types.OrchestratorRoutesResp, err error) {
	return NewAdminServicesLogic(l.ctx, l.svcCtx).OrchestratorRoutes(
		req.Authorization,
		req.IncludeDisabled,
		req.DebugUpstream,
	)
}
