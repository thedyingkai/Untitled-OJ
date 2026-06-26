package logic

import (
	"context"
	"encoding/json"
	"strings"

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
	topology, err := l.repo.Topology(l.ctx)
	if err != nil {
		return nil, err
	}
	return &types.ModuleTopologyResp{
		Sets:       setItems(topology.Sets),
		Nodes:      moduleItems(topology.Nodes, false),
		Edges:      edgeItems(topology.Edges),
		Components: componentItems(topology.Components),
	}, nil
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
