package logic

import (
	"context"
	"strings"

	"ojos-gateway/internal/kernel/serviceruntime"
	"ojos-gateway/internal/serviceregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/jackc/pgx/v5"
	"github.com/zeromicro/go-zero/core/logx"
)

type AdminRuntimeLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   serviceruntime.RegistryReader
}

func NewAdminRuntimeLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminRuntimeLogic {
	return &AdminRuntimeLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   serviceregistry.NewRepository(svcCtx.DB),
	}
}

func (l *AdminRuntimeLogic) ListServices(authHeader string) (*types.RuntimeServicesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := serviceruntime.BuildSnapshot(l.ctx, l.repo)
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

func (l *AdminRuntimeLogic) Reload(authHeader string) (*types.ServiceRuntimeRoutesResp, error) {
	servicesLogic := NewAdminServicesLogic(l.ctx, l.svcCtx)
	return servicesLogic.RuntimeRoutes(authHeader, false, true, false)
}

func (l *AdminRuntimeLogic) Operations(authHeader string) (*types.RuntimeOperationsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	rows, err := l.svcCtx.DB.Query(l.ctx, `
	SELECT operation_id, object_id AS service_id, action, status, actor_username,
       request, plan, result, error_message, created_at::text, updated_at::text
FROM service_runtime_operations
WHERE action LIKE 'runtime.%'
ORDER BY created_at DESC
LIMIT 100
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanRuntimeOperations(rows)
}

func (l *AdminRuntimeLogic) OperationDetail(authHeader string, operationID string) (*types.RuntimeOperationsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	rows, err := l.svcCtx.DB.Query(l.ctx, `
	SELECT operation_id, object_id AS service_id, action, status, actor_username,
       request, plan, result, error_message, created_at::text, updated_at::text
FROM service_runtime_operations
WHERE operation_id = $1 AND action LIKE 'runtime.%'
ORDER BY created_at DESC
LIMIT 1
`, strings.TrimSpace(operationID))
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	resp, err := scanRuntimeOperations(rows)
	if err != nil {
		return nil, err
	}
	if len(resp.Operations) == 0 {
		return nil, pgx.ErrNoRows
	}
	return resp, nil
}

func (l *AdminRuntimeLogic) ApplyDisabled(authHeader string) (*types.RuntimeApplyDisabledResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	return &types.RuntimeApplyDisabledResp{
		Code:    50100,
		Message: "runtime apply is intentionally disabled in Gateway/Web; use ojosctl/operator controlled apply",
	}, nil
}

func (l *AdminRuntimeLogic) plan(authHeader string, serviceID string, action string) (*types.RuntimeServicePlanResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := serviceruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	var plan serviceruntime.RuntimePlan
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

func (l *AdminRuntimeLogic) getService(serviceID string) (serviceruntime.RuntimeService, error) {
	snapshot, err := serviceruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return serviceruntime.RuntimeService{}, err
	}
	return l.driver().GetServiceState(l.ctx, snapshot, serviceID)
}

func (l *AdminRuntimeLogic) driver() serviceruntime.RuntimeDriver {
	if l.svcCtx.RuntimeDriver != nil {
		return l.svcCtx.RuntimeDriver
	}
	return serviceruntime.NewComposeDriver(l.svcCtx.RouteTableOptions.TrustedServices)
}

func runtimePlanItem(plan serviceruntime.RuntimePlan) types.RuntimePlanItem {
	commands := make([]types.RuntimePlanCommand, 0, len(plan.Commands))
	for _, command := range plan.Commands {
		commands = append(commands, types.RuntimePlanCommand{Kind: command.Kind, Argv: command.Argv})
	}
	return types.RuntimePlanItem{
		PlanId:               plan.PlanID,
		OperationId:          plan.OperationID,
		Action:               plan.Action,
		ServiceId:            plan.ServiceID,
		Driver:               plan.Driver,
		CanApply:             plan.CanApply,
		ApplyEnabled:         plan.ApplyEnabled,
		RequiresConfirmation: plan.RequiresConfirmation,
		DryRun:               plan.DryRun,
		AllowedTargets:       plan.AllowedTargets,
		Commands:             commands,
		Affected:             plan.Affected,
		BlockedBy:            plan.BlockedBy,
		Warnings:             plan.Warnings,
		CreatedAt:            plan.CreatedAt,
		ExpiresAt:            plan.ExpiresAt,
	}
}

func scanRuntimeOperations(rows pgx.Rows) (*types.RuntimeOperationsResp, error) {
	resp := &types.RuntimeOperationsResp{Operations: []types.RuntimeOperationItem{}}
	for rows.Next() {
		var item types.RuntimeOperationItem
		if err := rows.Scan(
			&item.OperationId,
			&item.ServiceId,
			&item.Action,
			&item.Status,
			&item.ActorUsername,
			&item.Request,
			&item.Plan,
			&item.Result,
			&item.ErrorMessage,
			&item.CreatedAt,
			&item.UpdatedAt,
		); err != nil {
			return nil, err
		}
		if plan, ok := item.Plan.(map[string]any); ok {
			if serviceID, ok := plan["service_id"].(string); ok {
				item.ServiceId = serviceID
			}
		}
		resp.Operations = append(resp.Operations, item)
	}
	return resp, rows.Err()
}
