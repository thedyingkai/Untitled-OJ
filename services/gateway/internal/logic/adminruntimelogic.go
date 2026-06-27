package logic

import (
	"context"
	"strings"

	"ojos-gateway/internal/kernel/moduleruntime"
	"ojos-gateway/internal/moduleregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminRuntimeLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   moduleruntime.RegistryReader
}

func NewAdminRuntimeLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminRuntimeLogic {
	return &AdminRuntimeLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   moduleregistry.NewRepository(svcCtx.DB),
	}
}

func (l *AdminRuntimeLogic) ListServices(authHeader string) (*types.RuntimeServicesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := moduleruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	services, err := l.driver().ListServices(l.ctx, snapshot)
	if err != nil {
		return nil, err
	}
	resp := &types.RuntimeServicesResp{}
	for _, service := range services {
		if service.Kind == "worker" {
			resp.Workers = append(resp.Workers, runtimeServiceItem(service))
		} else {
			resp.Services = append(resp.Services, runtimeServiceItem(service))
		}
	}
	return resp, nil
}

func (l *AdminRuntimeLogic) ServiceDetail(authHeader string, serviceID string) (*types.RuntimeServiceResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	service, err := l.getService(strings.TrimSpace(serviceID))
	if err != nil {
		return nil, err
	}
	return &types.RuntimeServiceResp{Service: runtimeServiceItem(service)}, nil
}

func (l *AdminRuntimeLogic) PlanStart(authHeader string, serviceID string) (*types.RuntimeServicePlanResp, error) {
	return l.plan(authHeader, serviceID, "start")
}

func (l *AdminRuntimeLogic) PlanStop(authHeader string, serviceID string) (*types.RuntimeServicePlanResp, error) {
	return l.plan(authHeader, serviceID, "stop")
}

func (l *AdminRuntimeLogic) PlanRestart(authHeader string, serviceID string) (*types.RuntimeServicePlanResp, error) {
	return l.plan(authHeader, serviceID, "restart")
}

func (l *AdminRuntimeLogic) PlanReload(authHeader string, serviceID string) (*types.RuntimeServicePlanResp, error) {
	return l.plan(authHeader, serviceID, "reload")
}

func (l *AdminRuntimeLogic) Reload(authHeader string) (*types.ModuleRuntimeRoutesResp, error) {
	modulesLogic := NewAdminModulesLogic(l.ctx, l.svcCtx)
	return modulesLogic.RuntimeRoutes(authHeader, false, true, false)
}

func (l *AdminRuntimeLogic) plan(authHeader string, serviceID string, action string) (*types.RuntimeServicePlanResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := moduleruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	var plan moduleruntime.RuntimePlan
	switch action {
	case "start":
		plan, err = l.driver().PlanStart(l.ctx, snapshot, serviceID)
	case "stop":
		plan, err = l.driver().PlanStop(l.ctx, snapshot, serviceID)
	case "restart":
		plan, err = l.driver().PlanRestart(l.ctx, snapshot, serviceID)
	case "reload":
		plan, err = l.driver().PlanReload(l.ctx, snapshot, serviceID)
	default:
		plan, err = l.driver().PlanHealth(l.ctx, snapshot, serviceID)
	}
	if err != nil {
		return nil, err
	}
	return &types.RuntimeServicePlanResp{Plan: runtimePlanItem(plan)}, nil
}

func (l *AdminRuntimeLogic) getService(serviceID string) (moduleruntime.RuntimeService, error) {
	snapshot, err := moduleruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return moduleruntime.RuntimeService{}, err
	}
	return l.driver().GetServiceState(l.ctx, snapshot, serviceID)
}

func (l *AdminRuntimeLogic) driver() moduleruntime.RuntimeDriver {
	if l.svcCtx.RuntimeDriver != nil {
		return l.svcCtx.RuntimeDriver
	}
	return moduleruntime.NewComposeDriver(l.svcCtx.RouteTableOptions.TrustedServices)
}

func runtimePlanItem(plan moduleruntime.RuntimePlan) types.RuntimePlanItem {
	commands := make([]types.RuntimePlanCommand, 0, len(plan.Commands))
	for _, command := range plan.Commands {
		commands = append(commands, types.RuntimePlanCommand{Tool: command.Tool, Args: command.Args})
	}
	return types.RuntimePlanItem{
		PlanId:       plan.PlanID,
		Action:       plan.Action,
		ServiceId:    plan.ServiceID,
		ModuleId:     plan.ModuleID,
		Driver:       plan.Driver,
		CanApply:     plan.CanApply,
		ApplyEnabled: plan.ApplyEnabled,
		Commands:     commands,
		Affected:     plan.Affected,
		BlockedBy:    plan.BlockedBy,
		Warnings:     plan.Warnings,
		CreatedAt:    plan.CreatedAt,
	}
}
