package logic

import (
	"context"
	"encoding/json"
	"strings"

	"ojos-gateway/internal/kernel/moduleruntime"
	"ojos-gateway/internal/moduleregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminModulesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	repo   moduleRegistryReader
}

type moduleRegistryReader interface {
	ListModules(context.Context) ([]moduleregistry.Module, error)
	ListSets(context.Context) ([]moduleregistry.Set, error)
	Topology(context.Context) (moduleregistry.Topology, error)
	Detail(context.Context, string) (moduleregistry.Detail, error)
	moduleruntime.RegistryReader
}

func NewAdminModulesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminModulesLogic {
	return &AdminModulesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		repo:   moduleregistry.NewRepository(svcCtx.DB),
	}
}

func (l *AdminModulesLogic) ListModules(authHeader string) (*types.ListModulesResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	modules, err := l.repo.ListModules(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListModulesResp{Modules: moduleItems(modules, false)}, nil
}

func (l *AdminModulesLogic) ListSets(authHeader string) (*types.ListModuleSetsResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	sets, err := l.repo.ListSets(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ListModuleSetsResp{Sets: setItems(sets)}, nil
}

func (l *AdminModulesLogic) Topology(authHeader string) (*types.ModuleTopologyResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	sets, err := l.repo.ListSets(l.ctx)
	if err != nil {
		return nil, err
	}
	snapshot, err := moduleruntime.BuildSnapshot(l.ctx, l.repo)
	if err != nil {
		return nil, err
	}
	components := runtimeAsComponentItems(snapshot.Components)
	return &types.ModuleTopologyResp{
		Sets:            setItems(sets),
		Nodes:           runtimeTopologyNodeItems(snapshot.Topology.Nodes),
		Edges:           runtimeTopologyEdgeItems(snapshot.Topology.Edges),
		Components:      components,
		ModuleNodes:     moduleItems(snapshot.Topology.ModuleNodes, false),
		DependencyEdges: edgeItems(snapshot.Topology.DependencyEdges),
	}, nil
}

func (l *AdminModulesLogic) RuntimeSnapshot(authHeader string, includeDisabled bool) (*types.ModuleRuntimeSnapshotResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	snapshot, err := moduleruntime.BuildSnapshotWithOptions(l.ctx, l.repo, moduleruntime.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	return runtimeSnapshotResp(snapshot), nil
}

func (l *AdminModulesLogic) RuntimeRouteTable(ctx context.Context) (moduleruntime.RouteTable, error) {
	snapshot, err := moduleruntime.BuildSnapshot(ctx, l.repo)
	if err != nil {
		return moduleruntime.RouteTable{}, err
	}
	return moduleruntime.BuildRouteTableWithOptions(snapshot, l.svcCtx.RouteTableOptions), nil
}

func (l *AdminModulesLogic) RuntimeRoutes(authHeader string, includeDisabled bool, reloaded bool, includeUpstream bool) (*types.ModuleRuntimeRoutesResp, error) {
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
	snapshot, err := moduleruntime.BuildSnapshotWithOptions(l.ctx, l.repo, moduleruntime.BuildOptions{
		IncludeDisabled: includeDisabled,
	})
	if err != nil {
		return nil, err
	}
	tableOptions := l.svcCtx.RouteTableOptions
	tableOptions.IncludeDisabledRoutes = includeDisabled
	table := moduleruntime.BuildRouteTableWithOptions(snapshot, tableOptions)
	resp := runtimeRoutesResp(table, includeUpstream)
	resp.Reloaded = reloaded
	return resp, nil
}

func (l *AdminModulesLogic) Detail(authHeader string, moduleID string) (*types.ModuleDetailResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}
	detail, err := l.repo.Detail(l.ctx, strings.TrimSpace(moduleID))
	if err != nil {
		return nil, err
	}
	return &types.ModuleDetailResp{
		Module:         moduleItem(detail.Module, true),
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

func setItems(items []moduleregistry.Set) []types.ModuleSetItem {
	result := make([]types.ModuleSetItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleSetItem{
			SetId:       item.SetID,
			Name:        item.Name,
			Description: item.Description,
			SortOrder:   item.SortOrder,
		})
	}
	return result
}

func moduleItems(items []moduleregistry.Module, includeManifest bool) []types.ModuleNodeItem {
	result := make([]types.ModuleNodeItem, 0, len(items))
	for _, item := range items {
		result = append(result, moduleItem(item, includeManifest))
	}
	return result
}

func moduleItem(item moduleregistry.Module, includeManifest bool) types.ModuleNodeItem {
	out := types.ModuleNodeItem{
		ModuleId:    item.ModuleID,
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

func edgeItems(items []moduleregistry.Edge) []types.ModuleEdgeItem {
	result := make([]types.ModuleEdgeItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleEdgeItem{
			FromModuleId:      item.FromModuleID,
			ToModuleId:        item.ToModuleID,
			EdgeType:          item.EdgeType,
			VersionConstraint: item.VersionConstraint,
			Required:          item.Required,
		})
	}
	return result
}

func componentItems(items []moduleregistry.Component) []types.ModuleComponentItem {
	result := make([]types.ModuleComponentItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleComponentItem{
			ModuleId:      item.ModuleID,
			ComponentId:   item.ComponentID,
			ComponentType: item.ComponentType,
			Status:        item.Status,
			Config:        rawJSON(item.Config),
		})
	}
	return result
}

func runtimeComponentItems(items []moduleruntime.RuntimeComponent) []types.ModuleRuntimeComponent {
	result := make([]types.ModuleRuntimeComponent, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleRuntimeComponent{
			ModuleId:    item.ModuleID,
			ComponentId: item.ComponentID,
			Type:        item.Type,
			Status:      item.Status,
			Config:      rawJSON(item.Config),
		})
	}
	return result
}

func runtimeManifestItems(items []moduleruntime.RuntimeManifestItem) []types.ModuleRuntimeManifestItem {
	result := make([]types.ModuleRuntimeManifestItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleRuntimeManifestItem{
			ModuleId: item.ModuleID,
			Id:       item.ID,
			Type:     item.Type,
			Status:   item.Status,
			Enabled:  item.Enabled,
			Config:   rawJSON(item.Config),
		})
	}
	return result
}

func runtimeTopologyNodeItems(items []moduleruntime.RuntimeTopologyNode) []types.ModuleRuntimeTopologyNode {
	result := make([]types.ModuleRuntimeTopologyNode, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleRuntimeTopologyNode{
			Id:       item.ID,
			ModuleId: item.ModuleID,
			Label:    item.Label,
			Type:     item.Type,
			Status:   item.Status,
			Source:   item.Source,
			Config:   rawJSON(item.Config),
		})
	}
	return result
}

func runtimeTopologyEdgeItems(items []moduleruntime.RuntimeTopologyEdge) []types.ModuleRuntimeTopologyEdge {
	result := make([]types.ModuleRuntimeTopologyEdge, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleRuntimeTopologyEdge{
			Id:       item.ID,
			ModuleId: item.ModuleID,
			From:     item.From,
			To:       item.To,
			Type:     item.Type,
			Required: item.Required,
			Source:   item.Source,
		})
	}
	return result
}

func runtimeAsComponentItems(items []moduleruntime.RuntimeComponent) []types.ModuleComponentItem {
	result := make([]types.ModuleComponentItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleComponentItem{
			ModuleId:      item.ModuleID,
			ComponentId:   item.ComponentID,
			ComponentType: item.Type,
			Status:        item.Status,
			Config:        rawJSON(item.Config),
		})
	}
	return result
}

func runtimeSnapshotResp(snapshot moduleruntime.Snapshot) *types.ModuleRuntimeSnapshotResp {
	return &types.ModuleRuntimeSnapshotResp{
		Version:        snapshot.Version,
		GeneratedAt:    snapshot.GeneratedAt,
		Modules:        moduleItems(snapshot.Modules, false),
		Permissions:    permissionItems(snapshot.Permissions),
		Roles:          runtimeManifestItems(snapshot.Roles),
		Menus:          menuItems(snapshot.Menus),
		FrontendRoutes: frontendRouteItems(snapshot.FrontendRoutes),
		GatewayRoutes:  gatewayRouteItems(snapshot.GatewayRoutes),
		Components:     runtimeComponentItems(snapshot.Components),
		Services:       runtimeComponentItems(snapshot.Services),
		Workers:        runtimeComponentItems(snapshot.Workers),
		StorageBuckets: runtimeManifestItems(snapshot.StorageBuckets),
		HealthChecks:   runtimeComponentItems(snapshot.HealthChecks),
		Operations:     runtimeManifestItems(snapshot.Operations),
		Topology: types.ModuleRuntimeTopology{
			Nodes:           runtimeTopologyNodeItems(snapshot.Topology.Nodes),
			Edges:           runtimeTopologyEdgeItems(snapshot.Topology.Edges),
			ModuleNodes:     moduleItems(snapshot.Topology.ModuleNodes, false),
			DependencyEdges: edgeItems(snapshot.Topology.DependencyEdges),
		},
		Warnings: snapshot.Warnings,
	}
}

func runtimeRoutesResp(table moduleruntime.RouteTable, includeUpstream bool) *types.ModuleRuntimeRoutesResp {
	routes := make([]types.ModuleRuntimeRouteItem, 0, len(table.Routes))
	for _, route := range table.Routes {
		upstream := ""
		if includeUpstream {
			upstream = route.UpstreamBase
		}
		routes = append(routes, types.ModuleRuntimeRouteItem{
			RouteId:       route.RouteID,
			ModuleId:      route.ModuleID,
			Prefix:        route.Prefix,
			ServiceId:     route.ServiceID,
			TargetService: route.TargetService,
			UpstreamBase:  upstream,
			AuthMode:      route.AuthMode,
			Methods:       route.Methods,
			Enabled:       route.Enabled,
			ProxyEnabled:  route.ProxyEnabled,
			Priority:      route.Priority,
			StripPrefix:   route.StripPrefix,
			RewritePrefix: route.RewritePrefix,
			HealthCheckId: route.HealthCheckID,
			CreatedFrom:   route.CreatedFrom,
			Status:        route.Status,
			Conflicts:     route.Conflicts,
			Warnings:      route.Warnings,
			BlockedBy:     route.BlockedBy,
		})
	}
	return &types.ModuleRuntimeRoutesResp{
		Version:     table.Version,
		GeneratedAt: table.GeneratedAt,
		Routes:      routes,
		Warnings:    table.Warnings,
		CanProxy:    table.CanProxy,
	}
}

func permissionItems(items []moduleregistry.Permission) []types.ModulePermissionItem {
	result := make([]types.ModulePermissionItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModulePermissionItem{
			ModuleId:      item.ModuleID,
			PermissionKey: item.PermissionKey,
			Description:   item.Description,
		})
	}
	return result
}

func menuItems(items []moduleregistry.Menu) []types.ModuleMenuItem {
	result := make([]types.ModuleMenuItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleMenuItem{
			ModuleId:           item.ModuleID,
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

func frontendRouteItems(items []moduleregistry.FrontendRoute) []types.ModuleFrontendRouteItem {
	result := make([]types.ModuleFrontendRouteItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleFrontendRouteItem{
			ModuleId:           item.ModuleID,
			RoutePath:          item.RoutePath,
			RouteName:          item.RouteName,
			ComponentKey:       item.ComponentKey,
			RequiredPermission: item.RequiredPermission,
			Enabled:            item.Enabled,
		})
	}
	return result
}

func gatewayRouteItems(items []moduleregistry.GatewayRoute) []types.ModuleGatewayRouteItem {
	result := make([]types.ModuleGatewayRouteItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleGatewayRouteItem{
			ModuleId:      item.ModuleID,
			Prefix:        item.Prefix,
			TargetService: item.TargetService,
			AuthMode:      item.AuthMode,
			Enabled:       item.Enabled,
		})
	}
	return result
}

func installationItems(items []moduleregistry.Installation) []types.ModuleInstallationItem {
	result := make([]types.ModuleInstallationItem, 0, len(items))
	for _, item := range items {
		result = append(result, types.ModuleInstallationItem{
			ModuleId:   item.ModuleID,
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
