package logic

import (
	"context"
	"strings"

	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServiceStatusLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   servicestatus.SnapshotReader
}

func NewAdminServiceStatusLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServiceStatusLogic {
	var repo servicestatus.SnapshotReader
	if svcCtx != nil {
		repo = svcCtx.Orchestrator
	}
	return &AdminServiceStatusLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   repo,
	}
}

func (l *AdminServiceStatusLogic) ListServices(authHeader string) (*types.ServiceStatusListResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := l.serviceSnapshot()
	if err != nil {
		return nil, err
	}
	services, err := l.driver().ListServices(l.ctx, snapshot)
	if err != nil {
		return nil, err
	}
	resp := &types.ServiceStatusListResp{}
	for _, service := range services {
		if service.Kind == "worker" {
			resp.Workers = append(resp.Workers, ServiceStatusItem(service))
		} else {
			resp.Services = append(resp.Services, ServiceStatusItem(service))
		}
	}
	return resp, nil
}

func (l *AdminServiceStatusLogic) ServiceDetail(authHeader string, serviceID string) (*types.ServiceStatusItemResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	service, err := l.getService(strings.TrimSpace(serviceID))
	if err != nil {
		return nil, err
	}
	return &types.ServiceStatusItemResp{Service: ServiceStatusItem(service)}, nil
}

func (l *AdminServiceStatusLogic) Operations(authHeader string) (*types.ServiceStatusOperationsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.svcCtx == nil || l.svcCtx.Orchestrator == nil {
		return nil, errOrchestratorUnavailable()
	}
	resp, err := l.svcCtx.Orchestrator.ServiceOperations(l.ctx, "")
	if err != nil {
		return nil, err
	}
	return serviceOperationsResp(resp), nil
}

func (l *AdminServiceStatusLogic) OperationDetail(authHeader string, operationID string) (*types.ServiceStatusOperationsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.svcCtx == nil || l.svcCtx.Orchestrator == nil {
		return nil, errOrchestratorUnavailable()
	}
	resp, err := l.svcCtx.Orchestrator.ServiceOperationDetail(l.ctx, strings.TrimSpace(operationID))
	if err != nil {
		return nil, err
	}
	return serviceOperationsResp(resp), nil
}

func (l *AdminServiceStatusLogic) getService(serviceID string) (servicestatus.ServiceStatus, error) {
	snapshot, err := l.serviceSnapshot()
	if err != nil {
		return servicestatus.ServiceStatus{}, err
	}
	return l.driver().GetServiceStatus(l.ctx, snapshot, serviceID)
}

func (l *AdminServiceStatusLogic) serviceSnapshot() (servicestatus.Snapshot, error) {
	if l == nil || l.repo == nil {
		return servicestatus.Snapshot{}, errOrchestratorUnavailable()
	}
	if client, ok := l.repo.(*orchestratorsnapshot.Client); ok {
		var snapshot servicestatus.Snapshot
		if err := client.DecodeOrchestratorSnapshot(l.ctx, false, &snapshot); err != nil {
			return servicestatus.Snapshot{}, err
		}
		return snapshot, nil
	}
	return servicestatus.BuildSnapshot(l.ctx, l.repo)
}

func (l *AdminServiceStatusLogic) driver() servicestatus.ServiceStatusDriver {
	if l.svcCtx.ServiceStatusDriver != nil {
		return l.svcCtx.ServiceStatusDriver
	}
	return servicestatus.NewComposeDriver(l.svcCtx.RouteTableOptions.TrustedServices)
}

func serviceOperationsResp(source orchestratorsnapshot.OperationsResponse) *types.ServiceStatusOperationsResp {
	resp := &types.ServiceStatusOperationsResp{Operations: []types.ServiceStatusOperationItem{}}
	for _, item := range source.Operations {
		resp.Operations = append(resp.Operations, types.ServiceStatusOperationItem{
			OperationId:   item.OperationID,
			ServiceId:     item.ServiceID,
			Action:        item.Action,
			Status:        item.Status,
			ActorUsername: item.ActorUsername,
			Request:       rawJSON(item.Request),
			Plan:          rawJSON(item.Plan),
			Result:        rawJSON(item.Result),
			ErrorMessage:  item.ErrorMessage,
			CreatedAt:     item.CreatedAt,
			UpdatedAt:     item.UpdatedAt,
		})
	}
	return resp
}
