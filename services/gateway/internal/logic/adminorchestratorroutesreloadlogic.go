// Code scaffolded by goctl. Safe to edit.

package logic

import (
	"context"
	"strings"

	"ojos-gateway/internal/orchestrator/servicestatus"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminOrchestratorRoutesReloadLogic struct {
	logx.Logger
	ctx         context.Context
	svcCtx      *svc.ServiceContext
	routeReader routeTableReader
}

type routeTableReader interface {
	ServiceRouteTable(context.Context) (servicestatus.RouteTable, error)
}

func NewAdminOrchestratorRoutesReloadLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminOrchestratorRoutesReloadLogic {
	return &AdminOrchestratorRoutesReloadLogic{
		Logger:      logx.WithContext(ctx),
		ctx:         ctx,
		svcCtx:      svcCtx,
		routeReader: NewAdminServicesLogic(ctx, svcCtx),
	}
}

func (l *AdminOrchestratorRoutesReloadLogic) AdminOrchestratorRoutesReload(req *types.AdminRoutesReloadReq) (*types.OrchestratorRoutesReloadResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, req.Authorization); err != nil {
		return nil, err
	}
	if l.svcCtx == nil || l.svcCtx.ServiceProxy == nil {
		return nil, errOrchestratorUnavailable()
	}
	reader := l.routeReader
	if reader == nil {
		reader = NewAdminServicesLogic(l.ctx, l.svcCtx)
	}
	table, err := l.svcCtx.ServiceProxy.Reload(l.ctx, reader)
	if err != nil {
		return nil, err
	}
	return &types.OrchestratorRoutesReloadResp{
		Status:      "reloaded",
		Message:     "gateway route table reloaded from orchestrator registry",
		OperationId: strings.TrimSpace(req.OperationId),
		ServiceName: strings.TrimSpace(req.ServiceName),
		RouteCount:  len(table.Routes),
	}, nil
}
