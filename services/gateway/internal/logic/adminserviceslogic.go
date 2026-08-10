package logic

import (
	"context"
	"encoding/json"
	"errors"
	"strings"

	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

func errOrchestratorUnavailable() error {
	return errors.New("orchestrator snapshot is unavailable")
}

type AdminServicesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   orchestratorSnapshotReader
}

type orchestratorSnapshotReader interface {
	ListServices(context.Context) ([]orchestratorsnapshot.Service, error)
	ListEndpointGroups(context.Context) ([]orchestratorsnapshot.EndpointGroup, error)
	Topology(context.Context) (orchestratorsnapshot.Topology, error)
	Detail(context.Context, string) (orchestratorsnapshot.Detail, error)
	servicestatus.SnapshotReader
}

func NewAdminServicesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServicesLogic {
	repo := orchestratorSnapshotReader(nil)
	if svcCtx != nil {
		repo = svcCtx.Orchestrator
	}
	return &AdminServicesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   repo,
	}
}

func (l *AdminServicesLogic) AdminServices(req *types.AdminAuthReq) (*types.ListServicesResp, error) {
	return l.ListServices(req.Authorization)
}

func (l *AdminServicesLogic) ListServices(authHeader string) (*types.ListServicesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	services, err := l.repo.ListServices(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListServicesResp{Services: serviceItems(services, false)}, nil
}

func (l *AdminServicesLogic) ListEndpointGroups(authHeader string) (*types.ListEndpointGroupsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	groups, err := l.repo.ListEndpointGroups(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListEndpointGroupsResp{EndpointGroups: endpointGroupItems(groups)}, nil
}

func (l *AdminServicesLogic) Topology(authHeader string) (*types.ServiceTopologyResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	groups, err := l.repo.ListEndpointGroups(l.ctx)
	if err != nil {
		return nil, err
	}
	snapshot, err := servicestatus.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	l.enrichOrchestratorSnapshot(&snapshot)
	components := serviceComponentsAsItems(snapshot.Components)
	return &types.ServiceTopologyResp{
		EndpointGroups:     endpointGroupItems(groups),
		Nodes:              serviceTopologyNodeItems(snapshot.Topology.Nodes),
		Edges:              serviceTopologyEdgeItems(snapshot.Topology.Edges),
		Components:         components,
		ServiceDefinitions: serviceItems(snapshot.Topology.ServiceDefinitions, false),
		DependencyEdges:    edgeItems(snapshot.Topology.DependencyEdges),
	}, nil
}

func (l *AdminServicesLogic) OrchestratorSnapshot(authHeader string, includeDisabled bool) (*types.OrchestratorSnapshotResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	if client, ok := l.repo.(*orchestratorsnapshot.Client); ok {
		var snapshot servicestatus.Snapshot
		if err := client.DecodeOrchestratorSnapshot(l.ctx, includeDisabled, &snapshot); err != nil {
			return nil, err
		}
		l.enrichOrchestratorSnapshot(&snapshot)
		return orchestratorSnapshotResp(snapshot), nil
	}
	snapshot, err := servicestatus.BuildSnapshotWithOptions(l.ctx, l.repo, servicestatus.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	l.enrichOrchestratorSnapshot(&snapshot)
	return orchestratorSnapshotResp(snapshot), nil
}

func (l *AdminServicesLogic) ServiceRouteTable(ctx context.Context) (servicestatus.RouteTable, error) {
	if l.repo == nil {
		return servicestatus.RouteTable{}, errOrchestratorUnavailable()
	}
	if client, ok := l.repo.(*orchestratorsnapshot.Client); ok && l.svcCtx != nil {
		nodeID := strings.TrimSpace(l.svcCtx.Config.Orchestrator.NodeID)
		if nodeID != "" {
			var table servicestatus.RouteTable
			if err := client.DecodeNodeOrchestratorRoutes(ctx, nodeID, true, &table); err != nil {
				return servicestatus.RouteTable{}, err
			}
			return table, nil
		}
	}
	snapshot, err := servicestatus.BuildSnapshot(ctx, l.repo)
	if err != nil {
		return servicestatus.RouteTable{}, err
	}
	l.enrichOrchestratorSnapshot(&snapshot)
	options := l.svcCtx.RouteTableOptions
	options.ServiceStatuses = servicestatus.ServiceStatusesByID(snapshot.Services)
	return servicestatus.BuildRouteTableWithOptions(snapshot, options), nil
}

func (l *AdminServicesLogic) OrchestratorRoutes(authHeader string, includeDisabled bool, includeUpstream bool) (*types.OrchestratorRoutesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	if client, ok := l.repo.(*orchestratorsnapshot.Client); ok {
		var table servicestatus.RouteTable
		nodeID := ""
		if l.svcCtx != nil {
			nodeID = strings.TrimSpace(l.svcCtx.Config.Orchestrator.NodeID)
		}
		var err error
		if nodeID != "" {
			err = client.DecodeNodeOrchestratorRoutes(l.ctx, nodeID, includeUpstream, &table)
		} else {
			err = client.DecodeOrchestratorRoutes(l.ctx, includeDisabled, includeUpstream, &table)
		}
		if err != nil {
			return nil, err
		}
		return orchestratorRoutesResp(table, includeUpstream), nil
	}
	snapshot, err := servicestatus.BuildSnapshotWithOptions(l.ctx, l.repo, servicestatus.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	l.enrichOrchestratorSnapshot(&snapshot)
	tableOptions := l.svcCtx.RouteTableOptions
	tableOptions.IncludeDisabledRoutes = includeDisabled
	tableOptions.ServiceStatuses = servicestatus.ServiceStatusesByID(snapshot.Services)
	table := servicestatus.BuildRouteTableWithOptions(snapshot, tableOptions)
	return orchestratorRoutesResp(table, includeUpstream), nil
}

func (l *AdminServicesLogic) enrichOrchestratorSnapshot(snapshot *servicestatus.Snapshot) {
	if l == nil || l.svcCtx == nil || snapshot == nil {
		return
	}
	driver := l.svcCtx.ServiceStatusDriver
	if driver == nil {
		return
	}
	services, err := driver.ListServices(l.ctx, *snapshot)
	if err != nil {
		snapshot.Warnings = append(snapshot.Warnings, "Service Status unavailable")
		return
	}
	workers := make([]servicestatus.ServiceStatus, 0, len(services))
	realServices := make([]servicestatus.ServiceStatus, 0, len(services))
	for _, service := range services {
		if service.Kind == "worker" {
			workers = append(workers, service)
		} else {
			realServices = append(realServices, service)
		}
	}
	snapshot.Services = realServices
	snapshot.Workers = workers
	snapshot.Topology.Nodes, snapshot.Topology.Edges = servicestatus.RebuildServiceTopology(*snapshot)
}

func (l *AdminServicesLogic) Detail(authHeader string, serviceID string) (*types.ServiceDetailResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if l.repo == nil {
		return nil, errOrchestratorUnavailable()
	}
	detail, err := l.repo.Detail(l.ctx, strings.TrimSpace(serviceID))
	if err != nil {
		return nil, err
	}
	return &types.ServiceDetailResp{
		Service:        serviceItem(detail.Service, true),
		Dependencies:   edgeItems(detail.Dependencies),
		Dependents:     edgeItems(detail.Dependents),
		Components:     componentItems(detail.Components),
		Permissions:    permissionItems(detail.Permissions),
		Menus:          menuItems(detail.Menus),
		FrontendRoutes: frontendRouteItems(detail.FrontendRoutes),
		GatewayRoutes:  gatewayRouteItems(detail.GatewayRoutes),
		Endpoints:      endpointItems(detail.Endpoints),
		HealthChecks:   componentItems(detail.HealthChecks),
	}, nil
}

func endpointGroupItems(items []orchestratorsnapshot.EndpointGroup) []types.EndpointGroupItem {
	result := make([]types.EndpointGroupItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.EndpointGroupItem{
			ServiceName:   item.ServiceName,
			Selector:      item.Selector,
			EndpointCount: item.EndpointCount,
			Endpoints:     item.Endpoints,
		})
	}
	return result
}

func serviceItems(items []orchestratorsnapshot.Service, includeManifest bool) []types.ServiceDefinitionItem {
	result := make([]types.ServiceDefinitionItem, 0, len(items))
	for _, item := range items {
		result = append(result, serviceItem(item, includeManifest))
	}
	return result
}

func serviceItem(item orchestratorsnapshot.Service, includeManifest bool) types.ServiceDefinitionItem {
	out := types.ServiceDefinitionItem{
		ServiceId:   item.ServiceID,
		Name:        item.Name,
		Version:     item.Version,
		Status:      item.Status,
		Kind:        item.Kind,
		Description: item.Description,
	}
	if includeManifest {
		out.Manifest = rawJSON(item.Manifest)
	}
	return out
}

func edgeItems(items []orchestratorsnapshot.Edge) []types.ServiceEdgeItem {
	result := make([]types.ServiceEdgeItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceEdgeItem{
			FromServiceId:     item.FromServiceID,
			ToServiceId:       item.ToServiceID,
			EdgeType:          item.EdgeType,
			VersionConstraint: item.VersionConstraint,
			Required:          item.Required,
		})
	}
	return result
}

func componentItems(items []orchestratorsnapshot.Component) []types.ServiceComponentItem {
	result := make([]types.ServiceComponentItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceComponentItem{
			ServiceId:     item.ServiceID,
			ComponentId:   item.ComponentID,
			ComponentType: item.ComponentType,
			Status:        item.Status,
			Config:        rawJSON(item.Config),
		})
	}
	return result
}

func serviceComponentItems(items []servicestatus.ServiceComponent) []types.ServiceStatusComponent {
	result := make([]types.ServiceStatusComponent, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceStatusComponent{
			ServiceId:   item.ServiceID,
			ComponentId: item.ComponentID,
			Type:        item.Type,
			Status:      item.Status,
			Config:      rawJSON(item.Config),
		})
	}
	return result
}

func ServiceStatusItems(items []servicestatus.ServiceStatus) []types.ServiceStatusItem {
	result := make([]types.ServiceStatusItem, 0, len(items))
	for _, item := range items {
		result = append(result, ServiceStatusItem(item))
	}
	return result
}

func ServiceStatusItem(item servicestatus.ServiceStatus) types.ServiceStatusItem {
	return types.ServiceStatusItem{
		OwnerServiceId: item.OwnerServiceID,
		ServiceId:      item.ServiceID,
		Name:           item.Name,
		Kind:           item.Kind,
		Lifecycle:      item.Lifecycle,
		Runtime:        item.Runtime,
		ComposeService: item.ComposeService,
		State:          item.State,
		Health:         item.Health,
		Required:       item.Required,
		Routes:         item.Routes,
		HealthCheckId:  item.HealthCheckID,
		Status:         item.Status,
		BlockedBy:      item.BlockedBy,
		Warnings:       item.Warnings,
	}
}

func serviceManifestItems(items []servicestatus.ServiceManifestItem) []types.OrchestratorSnapshotItem {
	result := make([]types.OrchestratorSnapshotItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.OrchestratorSnapshotItem{
			ServiceId: item.ServiceID,
			Id:        item.ID,
			Type:      item.Type,
			Status:    item.Status,
			Enabled:   item.Enabled,
			Config:    rawJSON(item.Config),
		})
	}
	return result
}

func serviceTopologyNodeItems(items []servicestatus.ServiceTopologyNode) []types.ServiceTopologyNode {
	result := make([]types.ServiceTopologyNode, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceTopologyNode{
			Id:        item.ID,
			ServiceId: item.ServiceID,
			Label:     item.Label,
			Type:      item.Type,
			Status:    item.Status,
			Source:    item.Source,
			Config:    rawJSON(item.Config),
		})
	}
	return result
}

func serviceTopologyEdgeItems(items []servicestatus.ServiceTopologyEdge) []types.ServiceTopologyEdge {
	result := make([]types.ServiceTopologyEdge, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceTopologyEdge{
			Id:        item.ID,
			ServiceId: item.ServiceID,
			From:      item.From,
			To:        item.To,
			Type:      item.Type,
			Required:  item.Required,
			Source:    item.Source,
		})
	}
	return result
}

func serviceComponentsAsItems(items []servicestatus.ServiceComponent) []types.ServiceComponentItem {
	result := make([]types.ServiceComponentItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceComponentItem{
			ServiceId:     item.ServiceID,
			ComponentId:   item.ComponentID,
			ComponentType: item.Type,
			Status:        item.Status,
			Config:        rawJSON(item.Config),
		})
	}
	return result
}

func orchestratorSnapshotResp(snapshot servicestatus.Snapshot) *types.OrchestratorSnapshotResp {
	return &types.OrchestratorSnapshotResp{
		Version:            snapshot.Version,
		GeneratedAt:        snapshot.GeneratedAt,
		ServiceDefinitions: serviceItems(snapshot.ServiceDefinitions, false),
		Permissions:        permissionItems(snapshot.Permissions),
		Roles:              serviceManifestItems(snapshot.Roles),
		Menus:              menuItems(snapshot.Menus),
		FrontendRoutes:     frontendRouteItems(snapshot.FrontendRoutes),
		GatewayRoutes:      gatewayRouteItems(snapshot.GatewayRoutes),
		Components:         serviceComponentItems(snapshot.Components),
		Services:           ServiceStatusItems(snapshot.Services),
		Workers:            ServiceStatusItems(snapshot.Workers),
		StorageBuckets:     serviceManifestItems(snapshot.StorageBuckets),
		HealthChecks:       serviceComponentItems(snapshot.HealthChecks),
		Operations:         serviceManifestItems(snapshot.Operations),
		Topology: types.ServiceTopologyGraph{
			Nodes:              serviceTopologyNodeItems(snapshot.Topology.Nodes),
			Edges:              serviceTopologyEdgeItems(snapshot.Topology.Edges),
			ServiceDefinitions: serviceItems(snapshot.Topology.ServiceDefinitions, false),
			DependencyEdges:    edgeItems(snapshot.Topology.DependencyEdges),
		},
		Warnings: snapshot.Warnings,
	}
}

func orchestratorRoutesResp(table servicestatus.RouteTable, includeUpstream bool) *types.OrchestratorRoutesResp {
	routes := make([]types.OrchestratorRouteItem, 0, len(table.Routes))
	for _, route := range table.Routes {
		upstream := ""
		if includeUpstream {
			upstream = route.UpstreamBase
		}
		routes = append(routes, types.OrchestratorRouteItem{
			RouteId:              route.RouteID,
			ApiId:                route.ApiID,
			BindingId:            route.BindingID,
			ConsumerDeploymentId: route.ConsumerDeploymentID,
			CredentialGeneration: route.CredentialGeneration,
			TimeoutMs:            route.TimeoutMS,
			NodeId:               route.NodeID,
			ProviderNodeId:       route.ProviderNodeID,
			ProviderHostIp:       route.ProviderHostIP,
			ProviderService:      route.ProviderService,
			ProviderEndpoint:     route.ProviderEndpoint,
			VisibilitySource:     route.VisibilitySource,
			Distance:             route.Distance,
			OwnerServiceId:       route.OwnerServiceID,
			Prefix:               route.Prefix,
			ServiceId:            route.ServiceID,
			TargetService:        route.TargetService,
			UpstreamBase:         upstream,
			AuthMode:             route.AuthMode,
			RequiredPermission:   route.RequiredPermission,
			Methods:              route.Methods,
			Enabled:              route.Enabled,
			ProxyEnabled:         route.ProxyEnabled,
			Priority:             route.Priority,
			StripPrefix:          route.StripPrefix,
			RewritePrefix:        route.RewritePrefix,
			HealthCheckId:        route.HealthCheckID,
			CreatedFrom:          route.CreatedFrom,
			Status:               route.Status,
			ServiceStatus:        route.ServiceStatus,
			ServiceHealth:        route.ServiceHealth,
			Conflicts:            route.Conflicts,
			Warnings:             route.Warnings,
			BlockedBy:            route.BlockedBy,
		})
	}
	return &types.OrchestratorRoutesResp{
		Version:     table.Version,
		GeneratedAt: table.GeneratedAt,
		Routes:      routes,
		Warnings:    table.Warnings,
		CanProxy:    table.CanProxy,
	}
}

func permissionItems(items []orchestratorsnapshot.Permission) []types.ServicePermissionItem {
	result := make([]types.ServicePermissionItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServicePermissionItem{
			ServiceId:     item.ServiceID,
			PermissionKey: item.PermissionKey,
			Description:   item.Description,
		})
	}
	return result
}

func menuItems(items []orchestratorsnapshot.Menu) []types.ServiceMenuItem {
	result := make([]types.ServiceMenuItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceMenuItem{
			ServiceId:          item.ServiceID,
			MenuKey:            item.MenuKey,
			Title:              item.Title,
			RoutePath:          item.RoutePath,
			Icon:               item.Icon,
			ParentKey:          item.ParentKey,
			SortOrder:          item.SortOrder,
			RequiredPermission: item.RequiredPermission,
			Enabled:            item.Enabled,
		})
	}
	return result
}

func frontendRouteItems(items []orchestratorsnapshot.FrontendRoute) []types.ServiceFrontendRouteItem {
	result := make([]types.ServiceFrontendRouteItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceFrontendRouteItem{
			ServiceId:          item.ServiceID,
			RoutePath:          item.RoutePath,
			RouteName:          item.RouteName,
			ComponentKey:       item.ComponentKey,
			RequiredPermission: item.RequiredPermission,
			Enabled:            item.Enabled,
		})
	}
	return result
}

func gatewayRouteItems(items []orchestratorsnapshot.GatewayRoute) []types.ServiceGatewayRouteItem {
	result := make([]types.ServiceGatewayRouteItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceGatewayRouteItem{
			ServiceId:          item.ServiceID,
			Prefix:             item.Prefix,
			TargetService:      item.TargetService,
			UpstreamBase:       item.UpstreamBase,
			AuthMode:           item.AuthMode,
			RequiredPermission: item.RequiredPermission,
			Enabled:            item.Enabled,
		})
	}
	return result
}

func endpointItems(items []orchestratorsnapshot.Endpoint) []types.ServiceEndpointItem {
	result := make([]types.ServiceEndpointItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceEndpointItem{
			Endpoint:    item.Endpoint,
			ServiceId:   item.ServiceID,
			Protocol:    item.Protocol,
			HealthPath:  item.HealthPath,
			Health:      item.Health,
			Reachable:   item.Reachable,
			DisplayName: item.DisplayName,
			Note:        item.Note,
			Config:      rawJSON(item.Config),
		})
	}
	return result
}

func rawJSON(data json.RawMessage) any {
	if len(data) == 0 {
		return map[string]any{}
	}
	var value any
	if err := json.Unmarshal(data, &value); err != nil {
		return map[string]any{}
	}
	return value
}
