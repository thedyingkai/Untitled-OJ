package moduleruntime

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
	"time"

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
	Version        string                         `json:"version"`
	GeneratedAt    string                         `json:"generated_at"`
	Modules        []moduleregistry.Module        `json:"modules"`
	Permissions    []moduleregistry.Permission    `json:"permissions"`
	Roles          []RuntimeManifestItem          `json:"roles"`
	Menus          []moduleregistry.Menu          `json:"menus"`
	FrontendRoutes []moduleregistry.FrontendRoute `json:"frontend_routes"`
	GatewayRoutes  []moduleregistry.GatewayRoute  `json:"gateway_routes"`
	Components     []RuntimeComponent             `json:"components"`
	Services       []RuntimeComponent             `json:"services"`
	Workers        []RuntimeComponent             `json:"workers"`
	StorageBuckets []RuntimeManifestItem          `json:"storage_buckets"`
	HealthChecks   []RuntimeComponent             `json:"health_checks"`
	Operations     []RuntimeManifestItem          `json:"operations"`
	Topology       RuntimeTopology                `json:"topology"`
	Warnings       []string                       `json:"warnings"`
}

type BuildOptions struct {
	IncludeDisabled bool
}

type RuntimeComponent struct {
	ModuleID    string          `json:"module_id"`
	ComponentID string          `json:"component_id"`
	Type        string          `json:"type"`
	Status      string          `json:"status"`
	Config      json.RawMessage `json:"config"`
}

type RuntimeManifestItem struct {
	ModuleID string          `json:"module_id"`
	ID       string          `json:"id"`
	Type     string          `json:"type"`
	Status   string          `json:"status"`
	Enabled  bool            `json:"enabled"`
	Config   json.RawMessage `json:"config"`
}

type RuntimeTopology struct {
	Nodes           []RuntimeTopologyNode   `json:"nodes"`
	Edges           []RuntimeTopologyEdge   `json:"edges"`
	DependencyEdges []moduleregistry.Edge   `json:"dependency_edges"`
	ModuleNodes     []moduleregistry.Module `json:"module_nodes"`
}

type RuntimeTopologyNode struct {
	ID       string          `json:"id"`
	ModuleID string          `json:"module_id"`
	Label    string          `json:"label"`
	Type     string          `json:"type"`
	Status   string          `json:"status"`
	Source   string          `json:"source"`
	Config   json.RawMessage `json:"config"`
}

type RuntimeTopologyEdge struct {
	ID       string `json:"id"`
	ModuleID string `json:"module_id"`
	From     string `json:"from"`
	To       string `json:"to"`
	Type     string `json:"type"`
	Required bool   `json:"required"`
	Source   string `json:"source"`
}

type RouteTable struct {
	Version     string         `json:"version"`
	GeneratedAt string         `json:"generated_at"`
	Routes      []RuntimeRoute `json:"routes"`
	Warnings    []string       `json:"warnings"`
	CanProxy    bool           `json:"can_proxy"`
}

type RuntimeRoute struct {
	ModuleID      string   `json:"module_id"`
	Prefix        string   `json:"prefix"`
	TargetService string   `json:"target_service"`
	AuthMode      string   `json:"auth_mode"`
	Enabled       bool     `json:"enabled"`
	Conflicts     []string `json:"conflicts"`
	Warnings      []string `json:"warnings"`
}

func BuildSnapshot(ctx context.Context, reader RegistryReader) (Snapshot, error) {
	return BuildSnapshotWithOptions(ctx, reader, BuildOptions{})
}

func BuildSnapshotWithOptions(ctx context.Context, reader RegistryReader, opts BuildOptions) (Snapshot, error) {
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

	visibleModules := moduleVisibility(modules, opts.IncludeDisabled)
	moduleByID := mapModules(modules)
	snapshot := Snapshot{
		Version:        "1",
		GeneratedAt:    time.Now().UTC().Format(time.RFC3339Nano),
		Modules:        filterModules(modules, visibleModules),
		Permissions:    filterPermissions(permissions, visibleModules),
		Menus:          filterMenus(menus, visibleModules),
		FrontendRoutes: filterFrontendRoutes(frontendRoutes, visibleModules),
		GatewayRoutes:  filterGatewayRoutes(gatewayRoutes, visibleModules),
		Topology: RuntimeTopology{
			ModuleNodes:     filterModules(modules, visibleModules),
			DependencyEdges: filterEdges(edges, visibleModules),
		},
	}
	for _, component := range components {
		if !visibleModules[component.ModuleID] {
			continue
		}
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
	snapshot.Roles = manifestItemsFromModules(snapshot.Modules, "roles")
	snapshot.StorageBuckets = storageBucketItems(snapshot.Modules, snapshot.Components)
	snapshot.Operations = manifestItemsFromModules(snapshot.Modules, "operations")
	snapshot.Topology.Nodes, snapshot.Topology.Edges = buildTopology(snapshot.Modules, moduleByID, snapshot.Components, snapshot.GatewayRoutes, snapshot.Menus, snapshot.FrontendRoutes, snapshot.HealthChecks, snapshot.Topology.DependencyEdges)
	snapshot.Warnings = append(snapshot.Warnings, topologyWarnings(snapshot.Topology)...)
	sortSnapshot(&snapshot)
	return snapshot, nil
}

func BuildRouteTable(snapshot Snapshot) RouteTable {
	table := RouteTable{
		Version:     snapshot.Version,
		GeneratedAt: time.Now().UTC().Format(time.RFC3339Nano),
		CanProxy:    false,
	}
	for _, route := range snapshot.GatewayRoutes {
		item := RuntimeRoute{
			ModuleID:      route.ModuleID,
			Prefix:        cleanPrefix(route.Prefix),
			TargetService: strings.TrimSpace(route.TargetService),
			AuthMode:      normalizeRouteAuthMode(route.AuthMode),
			Enabled:       route.Enabled,
		}
		if item.Prefix == "" {
			item.Warnings = append(item.Warnings, "empty prefix")
		}
		if item.TargetService == "" {
			item.Warnings = append(item.Warnings, "empty target service")
		}
		if !isSupportedRouteAuthMode(item.AuthMode) {
			item.Warnings = append(item.Warnings, "unsupported auth mode")
		}
		table.Routes = append(table.Routes, item)
	}

	for i := range table.Routes {
		for j := range table.Routes {
			if i == j {
				continue
			}
			left := table.Routes[i]
			right := table.Routes[j]
			if left.Prefix == "" || right.Prefix == "" {
				continue
			}
			if left.Prefix == right.Prefix {
				table.Routes[i].Conflicts = append(table.Routes[i].Conflicts, "duplicate prefix with "+right.ModuleID)
				continue
			}
			if routePrefixOverlaps(left.Prefix, right.Prefix) {
				table.Routes[i].Conflicts = append(table.Routes[i].Conflicts, "overlaps prefix "+right.Prefix+" from "+right.ModuleID)
			}
		}
	}

	for _, route := range table.Routes {
		if len(route.Conflicts) > 0 {
			table.Warnings = append(table.Warnings, route.ModuleID+" "+route.Prefix+": "+strings.Join(route.Conflicts, "; "))
		}
	}
	sortRouteTable(&table)
	return table
}

func moduleVisibility(items []moduleregistry.Module, includeDisabled bool) map[string]bool {
	visible := make(map[string]bool, len(items))
	for _, item := range items {
		if includeDisabled || item.Status == moduleregistry.StatusEnabled {
			visible[item.ModuleID] = true
		}
	}
	return visible
}

func mapModules(items []moduleregistry.Module) map[string]moduleregistry.Module {
	out := make(map[string]moduleregistry.Module, len(items))
	for _, item := range items {
		out[item.ModuleID] = item
	}
	return out
}

func filterModules(items []moduleregistry.Module, visible map[string]bool) []moduleregistry.Module {
	out := make([]moduleregistry.Module, 0, len(items))
	for _, item := range items {
		if visible[item.ModuleID] {
			out = append(out, item)
		}
	}
	return out
}

func filterPermissions(items []moduleregistry.Permission, visible map[string]bool) []moduleregistry.Permission {
	out := make([]moduleregistry.Permission, 0, len(items))
	for _, item := range items {
		if visible[item.ModuleID] {
			out = append(out, item)
		}
	}
	return out
}

func filterMenus(items []moduleregistry.Menu, visible map[string]bool) []moduleregistry.Menu {
	out := make([]moduleregistry.Menu, 0, len(items))
	for _, item := range items {
		if visible[item.ModuleID] {
			out = append(out, item)
		}
	}
	return out
}

func filterFrontendRoutes(items []moduleregistry.FrontendRoute, visible map[string]bool) []moduleregistry.FrontendRoute {
	out := make([]moduleregistry.FrontendRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ModuleID] {
			out = append(out, item)
		}
	}
	return out
}

func filterGatewayRoutes(items []moduleregistry.GatewayRoute, visible map[string]bool) []moduleregistry.GatewayRoute {
	out := make([]moduleregistry.GatewayRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ModuleID] {
			out = append(out, item)
		}
	}
	return out
}

func filterEdges(items []moduleregistry.Edge, visible map[string]bool) []moduleregistry.Edge {
	out := make([]moduleregistry.Edge, 0, len(items))
	for _, item := range items {
		if visible[item.FromModuleID] && visible[item.ToModuleID] {
			out = append(out, item)
		}
	}
	return out
}

type manifestEnvelope struct {
	Provides manifestProvides `json:"provides"`
}

type manifestProvides struct {
	Roles          []manifestNamedItem   `json:"roles"`
	StorageBuckets []manifestNamedItem   `json:"storage_buckets"`
	Events         manifestEvents        `json:"events"`
	ScheduledJobs  []manifestEnabledItem `json:"scheduled_jobs"`
	AdminPanels    []manifestAdminPanel  `json:"admin_panels"`
	Topology       manifestTopology      `json:"topology"`
}

type manifestNamedItem struct {
	ID          string `json:"id"`
	Key         string `json:"key"`
	Description string `json:"description"`
}

type manifestEnabledItem struct {
	ID          string `json:"id"`
	Description string `json:"description"`
	Enabled     bool   `json:"enabled"`
}

type manifestAdminPanel struct {
	ID                 string `json:"id"`
	RoutePath          string `json:"route_path"`
	RequiredPermission string `json:"required_permission"`
}

type manifestEvents struct {
	Publishes  []string `json:"publishes"`
	Subscribes []string `json:"subscribes"`
}

type manifestTopology struct {
	Nodes []manifestTopologyNode `json:"nodes"`
	Edges []manifestTopologyEdge `json:"edges"`
}

type manifestTopologyNode struct {
	ID    string `json:"id"`
	Type  string `json:"type"`
	Label string `json:"label"`
}

type manifestTopologyEdge struct {
	From string `json:"from"`
	To   string `json:"to"`
	Type string `json:"type"`
}

func manifestItemsFromModules(modules []moduleregistry.Module, kind string) []RuntimeManifestItem {
	items := make([]RuntimeManifestItem, 0)
	for _, module := range modules {
		manifest, ok := decodeManifest(module.Manifest)
		if !ok {
			continue
		}
		switch kind {
		case "roles":
			for _, role := range manifest.Provides.Roles {
				id := firstNonEmpty(role.Key, role.ID)
				if id == "" {
					continue
				}
				items = append(items, manifestItem(module.ModuleID, id, "role", module.Status, true, map[string]any{"description": role.Description}))
			}
		case "operations":
			for _, job := range manifest.Provides.ScheduledJobs {
				items = append(items, manifestItem(module.ModuleID, job.ID, "scheduled_job", module.Status, job.Enabled, map[string]any{"description": job.Description}))
			}
			for _, panel := range manifest.Provides.AdminPanels {
				items = append(items, manifestItem(module.ModuleID, panel.ID, "admin_panel", module.Status, true, map[string]any{"route_path": panel.RoutePath, "required_permission": panel.RequiredPermission}))
			}
			for _, event := range manifest.Provides.Events.Publishes {
				items = append(items, manifestItem(module.ModuleID, event, "event_publish", module.Status, true, map[string]any{}))
			}
			for _, event := range manifest.Provides.Events.Subscribes {
				items = append(items, manifestItem(module.ModuleID, event, "event_subscribe", module.Status, true, map[string]any{}))
			}
		}
	}
	return items
}

func storageBucketItems(modules []moduleregistry.Module, components []RuntimeComponent) []RuntimeManifestItem {
	items := make([]RuntimeManifestItem, 0)
	seen := map[string]bool{}
	for _, component := range components {
		if component.Type != "storage_bucket" {
			continue
		}
		key := component.ModuleID + "/" + component.ComponentID
		seen[key] = true
		items = append(items, RuntimeManifestItem{
			ModuleID: component.ModuleID,
			ID:       component.ComponentID,
			Type:     "storage_bucket",
			Status:   component.Status,
			Enabled:  component.Status == moduleregistry.StatusEnabled,
			Config:   component.Config,
		})
	}
	for _, module := range modules {
		manifest, ok := decodeManifest(module.Manifest)
		if !ok {
			continue
		}
		for _, bucket := range manifest.Provides.StorageBuckets {
			id := firstNonEmpty(bucket.ID, bucket.Key)
			if id == "" || seen[module.ModuleID+"/"+id] {
				continue
			}
			items = append(items, manifestItem(module.ModuleID, id, "storage_bucket", module.Status, module.Status == moduleregistry.StatusEnabled, map[string]any{"description": bucket.Description}))
		}
	}
	return items
}

func buildTopology(
	modules []moduleregistry.Module,
	moduleByID map[string]moduleregistry.Module,
	components []RuntimeComponent,
	gatewayRoutes []moduleregistry.GatewayRoute,
	menus []moduleregistry.Menu,
	frontendRoutes []moduleregistry.FrontendRoute,
	healthChecks []RuntimeComponent,
	dependencyEdges []moduleregistry.Edge,
) ([]RuntimeTopologyNode, []RuntimeTopologyEdge) {
	nodes := make([]RuntimeTopologyNode, 0)
	edges := make([]RuntimeTopologyEdge, 0)
	knownNodes := map[string]bool{}
	addNode := func(node RuntimeTopologyNode) {
		if node.ID == "" || knownNodes[node.ID] {
			return
		}
		knownNodes[node.ID] = true
		nodes = append(nodes, node)
	}
	for _, module := range modules {
		addNode(RuntimeTopologyNode{
			ID:       module.ModuleID,
			ModuleID: module.ModuleID,
			Label:    firstNonEmpty(module.Name, module.ModuleID),
			Type:     "module",
			Status:   module.Status,
			Source:   "registry",
			Config:   json.RawMessage(`{}`),
		})
	}
	for _, edge := range dependencyEdges {
		edges = append(edges, RuntimeTopologyEdge{
			ID:       edge.FromModuleID + "->" + edge.ToModuleID + ":" + edge.EdgeType,
			ModuleID: edge.FromModuleID,
			From:     edge.FromModuleID,
			To:       edge.ToModuleID,
			Type:     edge.EdgeType,
			Required: edge.Required,
			Source:   "registry",
		})
	}
	for _, component := range components {
		id := topologyID(component.ModuleID, "component", component.ComponentID)
		addNode(RuntimeTopologyNode{
			ID:       id,
			ModuleID: component.ModuleID,
			Label:    component.ComponentID,
			Type:     component.Type,
			Status:   component.Status,
			Source:   "registry",
			Config:   component.Config,
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:       component.ModuleID + "->" + id,
			ModuleID: component.ModuleID,
			From:     component.ModuleID,
			To:       id,
			Type:     "provides",
			Required: false,
			Source:   "registry",
		})
	}
	for _, route := range gatewayRoutes {
		id := topologyID(route.ModuleID, "gateway_route", route.Prefix)
		addNode(RuntimeTopologyNode{
			ID:       id,
			ModuleID: route.ModuleID,
			Label:    route.Prefix,
			Type:     "gateway_route",
			Status:   boolStatus(route.Enabled),
			Source:   "registry",
			Config:   mustRaw(map[string]any{"auth_mode": route.AuthMode, "target_service": route.TargetService}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:       route.ModuleID + "->" + id,
			ModuleID: route.ModuleID,
			From:     route.ModuleID,
			To:       id,
			Type:     "routes",
			Required: false,
			Source:   "registry",
		})
	}
	for _, menu := range menus {
		id := topologyID(menu.ModuleID, "menu", menu.MenuKey)
		addNode(RuntimeTopologyNode{
			ID:       id,
			ModuleID: menu.ModuleID,
			Label:    menu.Title,
			Type:     "menu",
			Status:   boolStatus(menu.Enabled),
			Source:   "registry",
			Config:   mustRaw(map[string]any{"route_path": menu.RoutePath, "required_permission": menu.RequiredPermission}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:       menu.ModuleID + "->" + id,
			ModuleID: menu.ModuleID,
			From:     menu.ModuleID,
			To:       id,
			Type:     "menu",
			Required: false,
			Source:   "registry",
		})
	}
	for _, route := range frontendRoutes {
		id := topologyID(route.ModuleID, "frontend_route", route.RoutePath)
		addNode(RuntimeTopologyNode{
			ID:       id,
			ModuleID: route.ModuleID,
			Label:    firstNonEmpty(route.RouteName, route.RoutePath),
			Type:     "frontend_route",
			Status:   boolStatus(route.Enabled),
			Source:   "registry",
			Config:   mustRaw(map[string]any{"route_path": route.RoutePath, "component_key": route.ComponentKey, "required_permission": route.RequiredPermission}),
		})
	}
	for _, health := range healthChecks {
		id := topologyID(health.ModuleID, "health", health.ComponentID)
		if knownNodes[id] {
			continue
		}
		addNode(RuntimeTopologyNode{
			ID:       id,
			ModuleID: health.ModuleID,
			Label:    health.ComponentID,
			Type:     "health_check",
			Status:   health.Status,
			Source:   "registry",
			Config:   health.Config,
		})
	}
	for _, module := range modules {
		manifest, ok := decodeManifest(module.Manifest)
		if !ok {
			continue
		}
		for _, item := range manifest.Provides.Topology.Nodes {
			id := topologyID(module.ModuleID, "manifest", item.ID)
			addNode(RuntimeTopologyNode{
				ID:       id,
				ModuleID: module.ModuleID,
				Label:    firstNonEmpty(item.Label, item.ID),
				Type:     item.Type,
				Status:   module.Status,
				Source:   "manifest",
				Config:   json.RawMessage(`{}`),
			})
			if moduleByID[module.ModuleID].ModuleID != "" {
				edges = append(edges, RuntimeTopologyEdge{
					ID:       module.ModuleID + "->" + id + ":declares",
					ModuleID: module.ModuleID,
					From:     module.ModuleID,
					To:       id,
					Type:     "declares",
					Required: false,
					Source:   "manifest",
				})
			}
		}
		for _, edge := range manifest.Provides.Topology.Edges {
			edges = append(edges, RuntimeTopologyEdge{
				ID:       topologyID(module.ModuleID, "manifest_edge", edge.From+"->"+edge.To+":"+edge.Type),
				ModuleID: module.ModuleID,
				From:     topologyID(module.ModuleID, "manifest", edge.From),
				To:       topologyID(module.ModuleID, "manifest", edge.To),
				Type:     edge.Type,
				Required: false,
				Source:   "manifest",
			})
		}
	}
	return nodes, edges
}

func topologyWarnings(topology RuntimeTopology) []string {
	known := map[string]bool{}
	for _, node := range topology.Nodes {
		known[node.ID] = true
	}
	warnings := make([]string, 0)
	for _, edge := range topology.Edges {
		if edge.From != "" && !known[edge.From] {
			warnings = append(warnings, "topology edge references missing source "+edge.From)
		}
		if edge.To != "" && !known[edge.To] {
			warnings = append(warnings, "topology edge references missing target "+edge.To)
		}
	}
	return warnings
}

func decodeManifest(raw json.RawMessage) (manifestEnvelope, bool) {
	if len(raw) == 0 {
		return manifestEnvelope{}, false
	}
	var manifest manifestEnvelope
	if err := json.Unmarshal(raw, &manifest); err != nil {
		return manifestEnvelope{}, false
	}
	return manifest, true
}

func manifestItem(moduleID string, id string, typ string, status string, enabled bool, config map[string]any) RuntimeManifestItem {
	return RuntimeManifestItem{
		ModuleID: moduleID,
		ID:       id,
		Type:     typ,
		Status:   status,
		Enabled:  enabled,
		Config:   mustRaw(config),
	}
}

func mustRaw(value any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		return json.RawMessage(`{}`)
	}
	return json.RawMessage(data)
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}

func boolStatus(enabled bool) string {
	if enabled {
		return moduleregistry.StatusEnabled
	}
	return "DISABLED"
}

func topologyID(moduleID string, kind string, value string) string {
	cleaned := strings.NewReplacer("/", "_", ":", "_", " ", "_").Replace(strings.TrimSpace(value))
	return moduleID + ":" + kind + ":" + cleaned
}

func cleanPrefix(prefix string) string {
	prefix = strings.TrimSpace(prefix)
	if prefix == "" {
		return ""
	}
	if !strings.HasPrefix(prefix, "/") {
		prefix = "/" + prefix
	}
	if len(prefix) > 1 {
		prefix = strings.TrimRight(prefix, "/")
	}
	return prefix
}

func normalizeRouteAuthMode(mode string) string {
	mode = strings.ToLower(strings.TrimSpace(mode))
	switch mode {
	case "":
		return "public"
	case "none":
		return "public"
	case "required", "optional":
		return "user"
	default:
		return mode
	}
}

func isSupportedRouteAuthMode(mode string) bool {
	switch mode {
	case "public", "user", "admin", "worker", "internal":
		return true
	default:
		return false
	}
}

func routePrefixOverlaps(left string, right string) bool {
	if left == right {
		return true
	}
	return strings.HasPrefix(left, right+"/") || strings.HasPrefix(right, left+"/")
}

func sortSnapshot(snapshot *Snapshot) {
	sort.Slice(snapshot.Modules, func(i, j int) bool { return snapshot.Modules[i].ModuleID < snapshot.Modules[j].ModuleID })
	sort.Slice(snapshot.Permissions, func(i, j int) bool {
		if snapshot.Permissions[i].ModuleID == snapshot.Permissions[j].ModuleID {
			return snapshot.Permissions[i].PermissionKey < snapshot.Permissions[j].PermissionKey
		}
		return snapshot.Permissions[i].ModuleID < snapshot.Permissions[j].ModuleID
	})
	sort.Slice(snapshot.Roles, func(i, j int) bool { return manifestLess(snapshot.Roles[i], snapshot.Roles[j]) })
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
	sort.Slice(snapshot.StorageBuckets, func(i, j int) bool { return manifestLess(snapshot.StorageBuckets[i], snapshot.StorageBuckets[j]) })
	sort.Slice(snapshot.Operations, func(i, j int) bool { return manifestLess(snapshot.Operations[i], snapshot.Operations[j]) })
	sort.Slice(snapshot.Topology.Nodes, func(i, j int) bool { return snapshot.Topology.Nodes[i].ID < snapshot.Topology.Nodes[j].ID })
	sort.Slice(snapshot.Topology.Edges, func(i, j int) bool { return snapshot.Topology.Edges[i].ID < snapshot.Topology.Edges[j].ID })
	sort.Slice(snapshot.Topology.ModuleNodes, func(i, j int) bool {
		return snapshot.Topology.ModuleNodes[i].ModuleID < snapshot.Topology.ModuleNodes[j].ModuleID
	})
	sort.Slice(snapshot.Topology.DependencyEdges, func(i, j int) bool {
		if snapshot.Topology.DependencyEdges[i].FromModuleID == snapshot.Topology.DependencyEdges[j].FromModuleID {
			return snapshot.Topology.DependencyEdges[i].ToModuleID < snapshot.Topology.DependencyEdges[j].ToModuleID
		}
		return snapshot.Topology.DependencyEdges[i].FromModuleID < snapshot.Topology.DependencyEdges[j].FromModuleID
	})
	sort.Strings(snapshot.Warnings)
}

func manifestLess(left RuntimeManifestItem, right RuntimeManifestItem) bool {
	if left.ModuleID == right.ModuleID {
		return left.ID < right.ID
	}
	return left.ModuleID < right.ModuleID
}

func sortRouteTable(table *RouteTable) {
	sort.Slice(table.Routes, func(i, j int) bool {
		if len(table.Routes[i].Prefix) == len(table.Routes[j].Prefix) {
			return table.Routes[i].Prefix < table.Routes[j].Prefix
		}
		return len(table.Routes[i].Prefix) > len(table.Routes[j].Prefix)
	})
	for i := range table.Routes {
		sort.Strings(table.Routes[i].Conflicts)
		sort.Strings(table.Routes[i].Warnings)
	}
	sort.Strings(table.Warnings)
}
