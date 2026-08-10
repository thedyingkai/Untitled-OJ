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
	if req.PushedRouteTable || len(req.Routes) > 0 {
		table := pushedRouteTable(req)
		l.svcCtx.ServiceProxy.SetRouteTable(table)
		return &types.OrchestratorRoutesReloadResp{
			Status:      "reloaded",
			Message:     "gateway route table reloaded from pushed orchestrator routes",
			OperationId: strings.TrimSpace(req.OperationId),
			ServiceName: strings.TrimSpace(req.ServiceName),
			RouteCount:  len(table.Routes),
		}, nil
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

func pushedRouteTable(req *types.AdminRoutesReloadReq) servicestatus.RouteTable {
	routes := make([]servicestatus.ServiceRoute, 0, len(req.Routes))
	for _, route := range req.Routes {
		routes = append(routes, servicestatus.ServiceRoute{
			RouteID:              route.RouteId,
			ApiID:                route.ApiId,
			BindingID:            route.BindingId,
			ConsumerDeploymentID: route.ConsumerDeploymentId,
			CredentialGeneration: route.CredentialGeneration,
			TimeoutMS:            route.TimeoutMs,
			NodeID:               route.NodeId,
			ProviderNodeID:       route.ProviderNodeId,
			ProviderHostIP:       route.ProviderHostIp,
			ProviderService:      route.ProviderService,
			ProviderEndpoint:     route.ProviderEndpoint,
			VisibilitySource:     route.VisibilitySource,
			Distance:             route.Distance,
			OwnerServiceID:       route.OwnerServiceId,
			Prefix:               route.Prefix,
			ServiceID:            route.ServiceId,
			TargetService:        route.TargetService,
			UpstreamBase:         route.UpstreamBase,
			AuthMode:             route.AuthMode,
			RequiredPermission:   route.RequiredPermission,
			Methods:              route.Methods,
			Enabled:              route.Enabled,
			ProxyEnabled:         route.ProxyEnabled,
			Priority:             route.Priority,
			StripPrefix:          route.StripPrefix,
			RewritePrefix:        route.RewritePrefix,
			HealthCheckID:        route.HealthCheckId,
			CreatedFrom:          route.CreatedFrom,
			Status:               route.Status,
			ServiceStatus:        route.ServiceStatus,
			ServiceHealth:        route.ServiceHealth,
			Conflicts:            route.Conflicts,
			Warnings:             route.Warnings,
			BlockedBy:            route.BlockedBy,
		})
	}
	version := strings.TrimSpace(req.Version)
	if version == "" {
		version = "1"
	}
	return servicestatus.RouteTable{
		Version:     version,
		GeneratedAt: strings.TrimSpace(req.GeneratedAt),
		Routes:      routes,
		Warnings:    req.Warnings,
		CanProxy:    req.CanProxy || len(routes) > 0,
	}
}
