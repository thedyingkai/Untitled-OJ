package logic

import (
	"context"
	"encoding/json"
	"strings"

	"ojos-gateway/internal/kernel/serviceruntime"
	"ojos-gateway/internal/serviceregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminServicesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   serviceRegistryReader
}

type serviceRegistryReader interface {
	ListServices(context.Context) ([]serviceregistry.Service, error)
	ListSets(context.Context) ([]serviceregistry.Set, error)
	Topology(context.Context) (serviceregistry.Topology, error)
	Detail(context.Context, string) (serviceregistry.Detail, error)
	serviceruntime.RegistryReader
}

func NewAdminServicesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminServicesLogic {
	return &AdminServicesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   serviceregistry.NewRepository(svcCtx.DB),
	}
}

func (l *AdminServicesLogic) ListServices(authHeader string) (*types.ListServicesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	services, err := l.repo.ListServices(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListServicesResp{Services: serviceItems(services, false)}, nil
}

func (l *AdminServicesLogic) ListSets(authHeader string) (*types.ListServiceSetsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	sets, err := l.repo.ListSets(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListServiceSetsResp{Sets: setItems(sets)}, nil
}

func (l *AdminServicesLogic) Topology(authHeader string) (*types.ServiceTopologyResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	sets, err := l.repo.ListSets(l.ctx)
	if err != nil {
		return nil, err
	}
	snapshot, err := serviceruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	l.enrichRuntimeSnapshot(&snapshot)
	components := runtimeAsComponentItems(snapshot.Components)
	return &types.ServiceTopologyResp{
		Sets:            setItems(sets),
		Nodes:           runtimeTopologyNodeItems(snapshot.Topology.Nodes),
		Edges:           runtimeTopologyEdgeItems(snapshot.Topology.Edges),
		Components:      components,
		ServiceNodes:    serviceItems(snapshot.Topology.ServiceNodes, false),
		DependencyEdges: edgeItems(snapshot.Topology.DependencyEdges),
	}, nil
}

func (l *AdminServicesLogic) RuntimeSnapshot(authHeader string, includeDisabled bool) (*types.ServiceRuntimeSnapshotResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := serviceruntime.BuildSnapshotWithOptions(l.ctx, l.repo, serviceruntime.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	l.enrichRuntimeSnapshot(&snapshot)
	return runtimeSnapshotResp(snapshot), nil
}

func (l *AdminServicesLogic) RuntimeRouteTable(ctx context.Context) (serviceruntime.RouteTable, error) {
	snapshot, err := serviceruntime.BuildSnapshot(ctx, l.repo)
	if err != nil {
		return serviceruntime.RouteTable{}, err
	}
	l.enrichRuntimeSnapshot(&snapshot)
	options := l.svcCtx.RouteTableOptions
	options.ServiceStates = serviceruntime.RuntimeServiceStates(snapshot.Services)
	return serviceruntime.BuildRouteTableWithOptions(snapshot, options), nil
}

func (l *AdminServicesLogic) RuntimeRoutes(authHeader string, includeDisabled bool, reloaded bool, includeUpstream bool) (*types.ServiceRuntimeRoutesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	if reloaded && l.svcCtx.RuntimeProxy != nil {
		activeTable, err := l.RuntimeRouteTable(l.ctx)
		if err != nil {
			return nil, err
		}
		l.svcCtx.RuntimeProxy.SetRouteTable(activeTable)
	}
	snapshot, err := serviceruntime.BuildSnapshotWithOptions(l.ctx, l.repo, serviceruntime.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	l.enrichRuntimeSnapshot(&snapshot)
	tableOptions := l.svcCtx.RouteTableOptions
	tableOptions.IncludeDisabledRoutes = includeDisabled
	tableOptions.ServiceStates = serviceruntime.RuntimeServiceStates(snapshot.Services)
	table := serviceruntime.BuildRouteTableWithOptions(snapshot, tableOptions)
	resp := runtimeRoutesResp(table, includeUpstream)
	resp.Reloaded = reloaded
	return resp, nil
}

func (l *AdminServicesLogic) enrichRuntimeSnapshot(snapshot *serviceruntime.Snapshot) {
	if l == nil || l.svcCtx == nil || snapshot == nil {
		return
	}
	driver := l.svcCtx.RuntimeDriver
	if driver == nil {
		return
	}
	services, err := driver.ListServices(l.ctx, *snapshot)
	if err != nil {
		snapshot.Warnings = append(snapshot.Warnings, "runtime services unavailable")
		return
	}
	workers := make([]serviceruntime.RuntimeService, 0, len(services))
	realServices := make([]serviceruntime.RuntimeService, 0, len(services))
	for _, service := range services {
		if service.Kind == "worker" {
			workers = append(workers, service)
		} else {
			realServices = append(realServices, service)
		}
	}
	snapshot.Services = realServices
	snapshot.Workers = workers
	snapshot.Topology.Nodes, snapshot.Topology.Edges = serviceruntime.RebuildRuntimeTopology(*snapshot)
}

func (l *AdminServicesLogic) Detail(authHeader string, serviceID string) (*types.ServiceDetailResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
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
		Installations:  installationItems(detail.Installations),
		HealthChecks:   componentItems(detail.HealthChecks),
	}, nil
}

func setItems(items []serviceregistry.Set) []types.ServiceSetItem {
	result := make([]types.ServiceSetItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceSetItem{
			SetId:       item.SetID,
			Name:        item.Name,
			Description: item.Description,
			SortOrder:   item.SortOrder,
		})
	}
	return result
}

func serviceItems(items []serviceregistry.Service, includeManifest bool) []types.ServiceNodeItem {
	result := make([]types.ServiceNodeItem, 0, len(items))
	for _, item := range items {
		result = append(result, serviceItem(item, includeManifest))
	}
	return result
}

func serviceItem(item serviceregistry.Service, includeManifest bool) types.ServiceNodeItem {
	out := types.ServiceNodeItem{
		ServiceId:   item.ServiceID,
		SetId:       item.SetID,
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

func edgeItems(items []serviceregistry.Edge) []types.ServiceEdgeItem {
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

func componentItems(items []serviceregistry.Component) []types.ServiceComponentItem {
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

func runtimeComponentItems(items []serviceruntime.RuntimeComponent) []types.ServiceRuntimeComponent {
	result := make([]types.ServiceRuntimeComponent, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceRuntimeComponent{
			ServiceId:   item.ServiceID,
			ComponentId: item.ComponentID,
			Type:        item.Type,
			Status:      item.Status,
			Config:      rawJSON(item.Config),
		})
	}
	return result
}

func runtimeServiceItems(items []serviceruntime.RuntimeService) []types.ServiceRuntimeService {
	result := make([]types.ServiceRuntimeService, 0, len(items))
	for _, item := range items {
		result = append(result, runtimeServiceItem(item))
	}
	return result
}

func runtimeServiceItem(item serviceruntime.RuntimeService) types.ServiceRuntimeService {
	return types.ServiceRuntimeService{
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

func runtimeManifestItems(items []serviceruntime.RuntimeManifestItem) []types.ServiceRuntimeManifestItem {
	result := make([]types.ServiceRuntimeManifestItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceRuntimeManifestItem{
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

func runtimeTopologyNodeItems(items []serviceruntime.RuntimeTopologyNode) []types.ServiceRuntimeTopologyNode {
	result := make([]types.ServiceRuntimeTopologyNode, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceRuntimeTopologyNode{
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

func runtimeTopologyEdgeItems(items []serviceruntime.RuntimeTopologyEdge) []types.ServiceRuntimeTopologyEdge {
	result := make([]types.ServiceRuntimeTopologyEdge, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceRuntimeTopologyEdge{
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

func runtimeAsComponentItems(items []serviceruntime.RuntimeComponent) []types.ServiceComponentItem {
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

func runtimeSnapshotResp(snapshot serviceruntime.Snapshot) *types.ServiceRuntimeSnapshotResp {
	return &types.ServiceRuntimeSnapshotResp{
		Version:        snapshot.Version,
		GeneratedAt:    snapshot.GeneratedAt,
		ServiceNodes:   serviceItems(snapshot.ServiceNodes, false),
		Permissions:    permissionItems(snapshot.Permissions),
		Roles:          runtimeManifestItems(snapshot.Roles),
		Menus:          menuItems(snapshot.Menus),
		FrontendRoutes: frontendRouteItems(snapshot.FrontendRoutes),
		GatewayRoutes:  gatewayRouteItems(snapshot.GatewayRoutes),
		Components:     runtimeComponentItems(snapshot.Components),
		Services:       runtimeServiceItems(snapshot.Services),
		Workers:        runtimeServiceItems(snapshot.Workers),
		StorageBuckets: runtimeManifestItems(snapshot.StorageBuckets),
		HealthChecks:   runtimeComponentItems(snapshot.HealthChecks),
		Operations:     runtimeManifestItems(snapshot.Operations),
		Topology: types.ServiceRuntimeTopology{
			Nodes:           runtimeTopologyNodeItems(snapshot.Topology.Nodes),
			Edges:           runtimeTopologyEdgeItems(snapshot.Topology.Edges),
			ServiceNodes:    serviceItems(snapshot.Topology.ServiceNodes, false),
			DependencyEdges: edgeItems(snapshot.Topology.DependencyEdges),
		},
		Warnings: snapshot.Warnings,
	}
}

func runtimeRoutesResp(table serviceruntime.RouteTable, includeUpstream bool) *types.ServiceRuntimeRoutesResp {
	routes := make([]types.ServiceRuntimeRouteItem, 0, len(table.Routes))
	for _, route := range table.Routes {
		upstream := ""
		if includeUpstream {
			upstream = route.UpstreamBase
		}
		routes = append(routes, types.ServiceRuntimeRouteItem{
			RouteId:        route.RouteID,
			OwnerServiceId: route.OwnerServiceID,
			Prefix:         route.Prefix,
			ServiceId:      route.ServiceID,
			TargetService:  route.TargetService,
			UpstreamBase:   upstream,
			AuthMode:       route.AuthMode,
			Methods:        route.Methods,
			Enabled:        route.Enabled,
			ProxyEnabled:   route.ProxyEnabled,
			Priority:       route.Priority,
			StripPrefix:    route.StripPrefix,
			RewritePrefix:  route.RewritePrefix,
			HealthCheckId:  route.HealthCheckID,
			CreatedFrom:    route.CreatedFrom,
			Status:         route.Status,
			ServiceState:   route.ServiceState,
			ServiceHealth:  route.ServiceHealth,
			Conflicts:      route.Conflicts,
			Warnings:       route.Warnings,
			BlockedBy:      route.BlockedBy,
		})
	}
	return &types.ServiceRuntimeRoutesResp{
		Version:     table.Version,
		GeneratedAt: table.GeneratedAt,
		Routes:      routes,
		Warnings:    table.Warnings,
		CanProxy:    table.CanProxy,
	}
}

func permissionItems(items []serviceregistry.Permission) []types.ServicePermissionItem {
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

func menuItems(items []serviceregistry.Menu) []types.ServiceMenuItem {
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

func frontendRouteItems(items []serviceregistry.FrontendRoute) []types.ServiceFrontendRouteItem {
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

func gatewayRouteItems(items []serviceregistry.GatewayRoute) []types.ServiceGatewayRouteItem {
	result := make([]types.ServiceGatewayRouteItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceGatewayRouteItem{
			ServiceId:     item.ServiceID,
			Prefix:        item.Prefix,
			TargetService: item.TargetService,
			AuthMode:      item.AuthMode,
			Enabled:       item.Enabled,
		})
	}
	return result
}

func installationItems(items []serviceregistry.Installation) []types.ServiceInstallationItem {
	result := make([]types.ServiceInstallationItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ServiceInstallationItem{
			ServiceId:  item.ServiceID,
			Name:       item.Name,
			Version:    item.Version,
			Status:     item.Status,
			Manifest:   rawJSON(item.Manifest),
			EnabledAt:  item.EnabledAt,
			DisabledAt: item.DisabledAt,
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
