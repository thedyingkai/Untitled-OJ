package moduleruntime

import (
	"context"
	"encoding/json"
	"sort"

	"ojos-gateway/internal/moduleregistry"
)

type RegistryReader interface {
	ListModules(context.Context) ([]moduleregistry.Module, error)
	ListPermissions(context.Context) ([]moduleregistry.Permission, error)
	ListMenus(context.Context) ([]moduleregistry.Menu, error)
	ListFrontendRoutes(context.Context) ([]moduleregistry.FrontendRoute, error)
	ListGatewayRoutes(context.Context) ([]moduleregistry.GatewayRoute, error)
	ListComponents(context.Context) ([]moduleregistry.Component, error)
	ListEdges(context.Context) ([]moduleregistry.Edge, error)
}

type Snapshot struct {
	Modules        []moduleregistry.Module        `json:"modules"`
	Permissions    []moduleregistry.Permission    `json:"permissions"`
	Menus          []moduleregistry.Menu          `json:"menus"`
	FrontendRoutes []moduleregistry.FrontendRoute `json:"frontend_routes"`
	GatewayRoutes  []moduleregistry.GatewayRoute  `json:"gateway_routes"`
	Components     []RuntimeComponent             `json:"components"`
	Services       []RuntimeComponent             `json:"services"`
	Workers        []RuntimeComponent             `json:"workers"`
	HealthChecks   []RuntimeComponent             `json:"health_checks"`
	Topology       RuntimeTopology                `json:"topology"`
}

type RuntimeComponent struct {
	ModuleID    string          `json:"module_id"`
	ComponentID string          `json:"component_id"`
	Type        string          `json:"type"`
	Status      string          `json:"status"`
	Config      json.RawMessage `json:"config"`
}

type RuntimeTopology struct {
	Nodes []moduleregistry.Module `json:"nodes"`
	Edges []moduleregistry.Edge   `json:"edges"`
}

func BuildSnapshot(ctx context.Context, reader RegistryReader) (Snapshot, error) {
	modules, err := reader.ListModules(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	permissions, err := reader.ListPermissions(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	menus, err := reader.ListMenus(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	frontendRoutes, err := reader.ListFrontendRoutes(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	gatewayRoutes, err := reader.ListGatewayRoutes(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	components, err := reader.ListComponents(ctx)
	if err != nil {
		return Snapshot{}, err
	}
	edges, err := reader.ListEdges(ctx)
	if err != nil {
		return Snapshot{}, err
	}

	snapshot := Snapshot{
		Modules:        enabledModules(modules),
		Permissions:    permissions,
		Menus:          enabledMenus(menus),
		FrontendRoutes: enabledFrontendRoutes(frontendRoutes),
		GatewayRoutes:  enabledGatewayRoutes(gatewayRoutes),
		Topology: RuntimeTopology{
			Nodes: modules,
			Edges: edges,
		},
	}
	for _, component := range components {
		item := RuntimeComponent{
			ModuleID:    component.ModuleID,
			ComponentID: component.ComponentID,
			Type:        component.ComponentType,
			Status:      component.Status,
			Config:      component.Config,
		}
		snapshot.Components = append(snapshot.Components, item)
		switch component.ComponentType {
		case "backend_service":
			snapshot.Services = append(snapshot.Services, item)
		case "worker_service":
			snapshot.Workers = append(snapshot.Workers, item)
		case "health_check":
			snapshot.HealthChecks = append(snapshot.HealthChecks, item)
		}
	}
	sortSnapshot(&snapshot)
	return snapshot, nil
}

func enabledModules(items []moduleregistry.Module) []moduleregistry.Module {
	out := make([]moduleregistry.Module, 0, len(items))
	for _, item := range items {
		if item.Status == moduleregistry.StatusEnabled {
			out = append(out, item)
		}
	}
	return out
}

func enabledMenus(items []moduleregistry.Menu) []moduleregistry.Menu {
	out := make([]moduleregistry.Menu, 0, len(items))
	for _, item := range items {
		if item.Enabled {
			out = append(out, item)
		}
	}
	return out
}

func enabledFrontendRoutes(items []moduleregistry.FrontendRoute) []moduleregistry.FrontendRoute {
	out := make([]moduleregistry.FrontendRoute, 0, len(items))
	for _, item := range items {
		if item.Enabled {
			out = append(out, item)
		}
	}
	return out
}

func enabledGatewayRoutes(items []moduleregistry.GatewayRoute) []moduleregistry.GatewayRoute {
	out := make([]moduleregistry.GatewayRoute, 0, len(items))
	for _, item := range items {
		if item.Enabled {
			out = append(out, item)
		}
	}
	return out
}

func sortSnapshot(snapshot *Snapshot) {
	sort.Slice(snapshot.Modules, func(i, j int) bool { return snapshot.Modules[i].ModuleID < snapshot.Modules[j].ModuleID })
	sort.Slice(snapshot.Menus, func(i, j int) bool {
		if snapshot.Menus[i].SortOrder == snapshot.Menus[j].SortOrder {
			return snapshot.Menus[i].MenuKey < snapshot.Menus[j].MenuKey
		}
		return snapshot.Menus[i].SortOrder < snapshot.Menus[j].SortOrder
	})
	sort.Slice(snapshot.FrontendRoutes, func(i, j int) bool {
		return snapshot.FrontendRoutes[i].RoutePath < snapshot.FrontendRoutes[j].RoutePath
	})
	sort.Slice(snapshot.GatewayRoutes, func(i, j int) bool { return snapshot.GatewayRoutes[i].Prefix < snapshot.GatewayRoutes[j].Prefix })
	sort.Slice(snapshot.Components, func(i, j int) bool {
		if snapshot.Components[i].ModuleID == snapshot.Components[j].ModuleID {
			return snapshot.Components[i].ComponentID < snapshot.Components[j].ComponentID
		}
		return snapshot.Components[i].ModuleID < snapshot.Components[j].ModuleID
	})
	sort.Slice(snapshot.Services, func(i, j int) bool { return snapshot.Services[i].ComponentID < snapshot.Services[j].ComponentID })
	sort.Slice(snapshot.Workers, func(i, j int) bool { return snapshot.Workers[i].ComponentID < snapshot.Workers[j].ComponentID })
	sort.Slice(snapshot.HealthChecks, func(i, j int) bool {
		return snapshot.HealthChecks[i].ComponentID < snapshot.HealthChecks[j].ComponentID
	})
}
