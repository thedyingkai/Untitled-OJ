package serviceruntime

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
	"time"

	"ojos-gateway/internal/serviceregistry"
)

type RegistryReader interface {
	ListServices(context.Context) ([]serviceregistry.Service, error)
	ListPermissions(context.Context) ([]serviceregistry.Permission, error)
	ListMenus(context.Context) ([]serviceregistry.Menu, error)
	ListFrontendRoutes(context.Context) ([]serviceregistry.FrontendRoute, error)
	ListGatewayRoutes(context.Context) ([]serviceregistry.GatewayRoute, error)
	ListComponents(context.Context) ([]serviceregistry.Component, error)
	ListEdges(context.Context) ([]serviceregistry.Edge, error)
}

type Snapshot struct {
	Version        string                          `json:"version"`
	GeneratedAt    string                          `json:"generated_at"`
	ServiceNodes   []serviceregistry.Service       `json:"service_nodes"`
	Permissions    []serviceregistry.Permission    `json:"permissions"`
	Roles          []RuntimeManifestItem           `json:"roles"`
	Menus          []serviceregistry.Menu          `json:"menus"`
	FrontendRoutes []serviceregistry.FrontendRoute `json:"frontend_routes"`
	GatewayRoutes  []serviceregistry.GatewayRoute  `json:"gateway_routes"`
	Components     []RuntimeComponent              `json:"components"`
	Services       []RuntimeService                `json:"services"`
	Workers        []RuntimeService                `json:"workers"`
	StorageBuckets []RuntimeManifestItem           `json:"storage_buckets"`
	HealthChecks   []RuntimeComponent              `json:"health_checks"`
	Operations     []RuntimeManifestItem           `json:"operations"`
	Topology       RuntimeTopology                 `json:"topology"`
	Warnings       []string                        `json:"warnings"`
}

type BuildOptions struct {
	IncludeDisabled bool
}

type RuntimeComponent struct {
	ServiceID   string          `json:"service_id"`
	ComponentID string          `json:"component_id"`
	Type        string          `json:"type"`
	Status      string          `json:"status"`
	Config      json.RawMessage `json:"config"`
}

type RuntimeManifestItem struct {
	ServiceID string          `json:"service_id"`
	ID        string          `json:"id"`
	Type      string          `json:"type"`
	Status    string          `json:"status"`
	Enabled   bool            `json:"enabled"`
	Config    json.RawMessage `json:"config"`
}

type RuntimeTopology struct {
	Nodes           []RuntimeTopologyNode     `json:"nodes"`
	Edges           []RuntimeTopologyEdge     `json:"edges"`
	DependencyEdges []serviceregistry.Edge    `json:"dependency_edges"`
	ServiceNodes    []serviceregistry.Service `json:"service_nodes"`
}

type RuntimeTopologyNode struct {
	ID        string          `json:"id"`
	ServiceID string          `json:"service_id"`
	Label     string          `json:"label"`
	Type      string          `json:"type"`
	Status    string          `json:"status"`
	Source    string          `json:"source"`
	Config    json.RawMessage `json:"config"`
}

type RuntimeTopologyEdge struct {
	ID        string `json:"id"`
	ServiceID string `json:"service_id"`
	From      string `json:"from"`
	To        string `json:"to"`
	Type      string `json:"type"`
	Required  bool   `json:"required"`
	Source    string `json:"source"`
}

type RouteTable struct {
	Version     string         `json:"version"`
	GeneratedAt string         `json:"generated_at"`
	Routes      []RuntimeRoute `json:"routes"`
	Warnings    []string       `json:"warnings"`
	CanProxy    bool           `json:"can_proxy"`
}

type RuntimeRoute struct {
	RouteID        string   `json:"route_id"`
	OwnerServiceID string   `json:"owner_service_id"`
	Prefix         string   `json:"prefix"`
	ServiceID      string   `json:"service_id"`
	TargetService  string   `json:"target_service"`
	UpstreamBase   string   `json:"upstream_base,omitempty"`
	AuthMode       string   `json:"auth_mode"`
	Methods        []string `json:"methods"`
	Enabled        bool     `json:"enabled"`
	ProxyEnabled   bool     `json:"proxy_enabled"`
	Priority       int      `json:"priority"`
	StripPrefix    string   `json:"strip_prefix,omitempty"`
	RewritePrefix  string   `json:"rewrite_prefix,omitempty"`
	HealthCheckID  string   `json:"health_check_id,omitempty"`
	CreatedFrom    string   `json:"created_from"`
	Status         string   `json:"status"`
	ServiceState   string   `json:"service_state,omitempty"`
	ServiceHealth  string   `json:"service_health,omitempty"`
	Conflicts      []string `json:"conflicts"`
	Warnings       []string `json:"warnings"`
	BlockedBy      []string `json:"blocked_by"`
}

type RouteTableOptions struct {
	TrustedServices       map[string]TrustedService
	ServiceStates         map[string]RuntimeService
	ReservedPrefixes      []string
	IncludeDisabledRoutes bool
}

type TrustedService struct {
	ServiceID     string
	UpstreamBase  string
	StripPrefix   string
	RewritePrefix string
	HealthCheckID string
}

func BuildSnapshot(ctx context.Context, reader RegistryReader) (Snapshot, error) {
	return BuildSnapshotWithOptions(ctx, reader, BuildOptions{})
}

func BuildSnapshotWithOptions(ctx context.Context, reader RegistryReader, opts BuildOptions) (Snapshot, error) {
	services, err := reader.ListServices(ctx)
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

	visibleServices := serviceVisibility(services, opts.IncludeDisabled)
	serviceByID := mapServices(services)
	snapshot := Snapshot{
		Version:        "1",
		GeneratedAt:    time.Now().UTC().Format(time.RFC3339Nano),
		ServiceNodes:   filterServices(services, visibleServices),
		Permissions:    filterPermissions(permissions, visibleServices),
		Menus:          filterMenus(menus, visibleServices),
		FrontendRoutes: filterFrontendRoutes(frontendRoutes, visibleServices),
		GatewayRoutes:  filterGatewayRoutes(gatewayRoutes, visibleServices),
		Topology: RuntimeTopology{
			ServiceNodes:    filterServices(services, visibleServices),
			DependencyEdges: filterEdges(edges, visibleServices),
		},
	}
	for _, component := range components {
		if !visibleServices[component.ServiceID] {
			continue
		}
		item := RuntimeComponent{
			ServiceID:   component.ServiceID,
			ComponentID: component.ComponentID,
			Type:        component.ComponentType,
			Status:      component.Status,
			Config:      component.Config,
		}
		snapshot.Components = append(snapshot.Components, item)
		switch component.ComponentType {
		case "health_check":
			snapshot.HealthChecks = append(snapshot.HealthChecks, item)
		}
	}
	snapshot.Roles = manifestItemsFromServices(snapshot.ServiceNodes, "roles")
	snapshot.StorageBuckets = storageBucketItems(snapshot.ServiceNodes, snapshot.Components)
	snapshot.Operations = manifestItemsFromServices(snapshot.ServiceNodes, "operations")
	snapshot.Services, snapshot.Workers = collectRuntimeServiceDeclarations(snapshot)
	snapshot.Topology.Nodes, snapshot.Topology.Edges = buildTopology(snapshot.ServiceNodes, serviceByID, snapshot.Components, snapshot.Services, snapshot.Workers, snapshot.GatewayRoutes, snapshot.Menus, snapshot.FrontendRoutes, snapshot.HealthChecks, snapshot.Topology.DependencyEdges)
	snapshot.Warnings = append(snapshot.Warnings, topologyWarnings(snapshot.Topology)...)
	sortSnapshot(&snapshot)
	return snapshot, nil
}

func BuildRouteTable(snapshot Snapshot) RouteTable {
	return BuildRouteTableWithOptions(snapshot, RouteTableOptions{})
}

func BuildRouteTableWithOptions(snapshot Snapshot, opts RouteTableOptions) RouteTable {
	table := RouteTable{
		Version:     snapshot.Version,
		GeneratedAt: time.Now().UTC().Format(time.RFC3339Nano),
		CanProxy:    false,
	}
	trusted := normalizeTrustedServices(opts.TrustedServices)
	reserved := normalizeReservedPrefixes(opts.ReservedPrefixes)
	for _, route := range snapshot.GatewayRoutes {
		if !route.Enabled && !opts.IncludeDisabledRoutes {
			continue
		}
		serviceID := strings.TrimSpace(route.TargetService)
		trustedService, serviceTrusted := trusted[serviceID]
		serviceState, hasServiceState := opts.ServiceStates[serviceID]
		item := RuntimeRoute{
			RouteID:        routeID(route.ServiceID, route.Prefix),
			OwnerServiceID: route.ServiceID,
			Prefix:         cleanPrefix(route.Prefix),
			ServiceID:      serviceID,
			TargetService:  serviceID,
			AuthMode:       normalizeRouteAuthMode(route.AuthMode),
			Methods:        defaultRouteMethods(),
			Enabled:        route.Enabled,
			Priority:       len(cleanPrefix(route.Prefix)),
			CreatedFrom:    "registry",
			Status:         "active",
		}
		if serviceTrusted {
			item.UpstreamBase = trustedService.UpstreamBase
			item.StripPrefix = cleanPrefix(trustedService.StripPrefix)
			item.RewritePrefix = cleanPrefix(trustedService.RewritePrefix)
			item.HealthCheckID = trustedService.HealthCheckID
		}
		if item.Prefix == "" {
			item.BlockedBy = append(item.BlockedBy, "empty prefix")
		}
		if item.ServiceID == "" {
			item.BlockedBy = append(item.BlockedBy, "empty service_id")
		}
		if !isSupportedRouteAuthMode(item.AuthMode) {
			item.BlockedBy = append(item.BlockedBy, "unsupported auth mode")
		}
		if !serviceTrusted {
			item.BlockedBy = append(item.BlockedBy, "unknown trusted service")
		}
		if reservedPrefixMatches(item.Prefix, reserved) {
			item.BlockedBy = append(item.BlockedBy, "reserved prefix")
		}
		structuralBlocked := len(item.BlockedBy) > 0
		if hasServiceState {
			item.ServiceState = serviceState.State
			item.ServiceHealth = serviceState.Health
			item.HealthCheckID = firstNonEmpty(item.HealthCheckID, serviceState.HealthCheckID)
			switch serviceState.State {
			case ServiceStateRunning:
			case ServiceStateDegraded:
				item.Status = "degraded"
				item.BlockedBy = append(item.BlockedBy, "service degraded")
				item.Warnings = append(item.Warnings, "service health is "+serviceState.Health)
			case ServiceStateDeclared, ServiceStateInstalled, ServiceStateEnabled, ServiceStateStarting:
				item.Status = "degraded"
				item.BlockedBy = append(item.BlockedBy, "service not running")
				item.Warnings = append(item.Warnings, "service state is "+serviceState.State)
			default:
				item.Status = "unavailable"
				item.BlockedBy = append(item.BlockedBy, "service not running")
				item.Warnings = append(item.Warnings, "service state is "+serviceState.State)
			}
		}
		if !item.Enabled {
			item.Status = "disabled"
		}
		if structuralBlocked {
			item.Status = "blocked"
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
				table.Routes[i].Conflicts = append(table.Routes[i].Conflicts, "duplicate prefix with "+right.ServiceID)
				continue
			}
			if routePrefixOverlaps(left.Prefix, right.Prefix) {
				table.Routes[i].Conflicts = append(table.Routes[i].Conflicts, "overlaps prefix "+right.Prefix+" from "+right.ServiceID)
			}
		}
	}

	for _, route := range table.Routes {
		if len(route.Conflicts) > 0 {
			for i := range table.Routes {
				if table.Routes[i].RouteID == route.RouteID {
					table.Routes[i].Warnings = append(table.Routes[i].Warnings, route.Conflicts...)
				}
			}
		}
	}

	for i := range table.Routes {
		if hasDuplicateConflict(table.Routes[i].Conflicts) {
			table.Routes[i].BlockedBy = append(table.Routes[i].BlockedBy, "duplicate prefix")
			table.Routes[i].Status = "blocked"
		}
		table.Routes[i].ProxyEnabled = table.Routes[i].Enabled &&
			table.Routes[i].Status == "active" &&
			len(table.Routes[i].BlockedBy) == 0
		if table.Routes[i].ProxyEnabled {
			table.CanProxy = true
		}
	}

	for _, route := range table.Routes {
		if len(route.BlockedBy) > 0 {
			table.Warnings = append(table.Warnings, route.ServiceID+" "+route.Prefix+": blocked by "+strings.Join(route.BlockedBy, "; "))
		}
		if len(route.Conflicts) > 0 {
			table.Warnings = append(table.Warnings, route.ServiceID+" "+route.Prefix+": "+strings.Join(route.Conflicts, "; "))
		}
	}
	sortRouteTable(&table)
	return table
}

func serviceVisibility(items []serviceregistry.Service, includeDisabled bool) map[string]bool {
	visible := make(map[string]bool, len(items))
	for _, item := range items {
		if includeDisabled || item.Status == serviceregistry.StatusEnabled {
			visible[item.ServiceID] = true
		}
	}
	return visible
}

func mapServices(items []serviceregistry.Service) map[string]serviceregistry.Service {
	out := make(map[string]serviceregistry.Service, len(items))
	for _, item := range items {
		out[item.ServiceID] = item
	}
	return out
}

func filterServices(items []serviceregistry.Service, visible map[string]bool) []serviceregistry.Service {
	out := make([]serviceregistry.Service, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterPermissions(items []serviceregistry.Permission, visible map[string]bool) []serviceregistry.Permission {
	out := make([]serviceregistry.Permission, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterMenus(items []serviceregistry.Menu, visible map[string]bool) []serviceregistry.Menu {
	out := make([]serviceregistry.Menu, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterFrontendRoutes(items []serviceregistry.FrontendRoute, visible map[string]bool) []serviceregistry.FrontendRoute {
	out := make([]serviceregistry.FrontendRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterGatewayRoutes(items []serviceregistry.GatewayRoute, visible map[string]bool) []serviceregistry.GatewayRoute {
	out := make([]serviceregistry.GatewayRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterEdges(items []serviceregistry.Edge, visible map[string]bool) []serviceregistry.Edge {
	out := make([]serviceregistry.Edge, 0, len(items))
	for _, item := range items {
		if visible[item.FromServiceID] && visible[item.ToServiceID] {
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
	Services       []manifestServiceItem `json:"services"`
	Workers        []manifestServiceItem `json:"workers"`
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

type manifestServiceItem struct {
	ID             string   `json:"id"`
	Name           string   `json:"name"`
	Kind           string   `json:"kind"`
	Lifecycle      string   `json:"lifecycle"`
	TrustedRuntime string   `json:"trusted_runtime"`
	ComposeService string   `json:"compose_service"`
	HealthCheckID  string   `json:"health_check_id"`
	Routes         []string `json:"routes"`
	Required       bool     `json:"required"`
	Path           string   `json:"path"`
	Health         string   `json:"health"`
	Exposure       string   `json:"exposure"`
	Mode           string   `json:"mode"`
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

func manifestItemsFromServices(services []serviceregistry.Service, kind string) []RuntimeManifestItem {
	items := make([]RuntimeManifestItem, 0)
	for _, service := range services {
		manifest, ok := decodeManifest(service.Manifest)
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
				items = append(items, manifestItem(service.ServiceID, id, "role", service.Status, true, map[string]any{"description": role.Description}))
			}
		case "operations":
			for _, job := range manifest.Provides.ScheduledJobs {
				items = append(items, manifestItem(service.ServiceID, job.ID, "scheduled_job", service.Status, job.Enabled, map[string]any{"description": job.Description}))
			}
			for _, panel := range manifest.Provides.AdminPanels {
				items = append(items, manifestItem(service.ServiceID, panel.ID, "admin_panel", service.Status, true, map[string]any{"route_path": panel.RoutePath, "required_permission": panel.RequiredPermission}))
			}
			for _, event := range manifest.Provides.Events.Publishes {
				items = append(items, manifestItem(service.ServiceID, event, "event_publish", service.Status, true, map[string]any{}))
			}
			for _, event := range manifest.Provides.Events.Subscribes {
				items = append(items, manifestItem(service.ServiceID, event, "event_subscribe", service.Status, true, map[string]any{}))
			}
		}
	}
	return items
}

func storageBucketItems(services []serviceregistry.Service, components []RuntimeComponent) []RuntimeManifestItem {
	items := make([]RuntimeManifestItem, 0)
	seen := map[string]bool{}
	for _, component := range components {
		if component.Type != "storage_bucket" {
			continue
		}
		key := component.ServiceID + "/" + component.ComponentID
		seen[key] = true
		items = append(items, RuntimeManifestItem{
			ServiceID: component.ServiceID,
			ID:        component.ComponentID,
			Type:      "storage_bucket",
			Status:    component.Status,
			Enabled:   component.Status == serviceregistry.StatusEnabled,
			Config:    component.Config,
		})
	}
	for _, service := range services {
		manifest, ok := decodeManifest(service.Manifest)
		if !ok {
			continue
		}
		for _, bucket := range manifest.Provides.StorageBuckets {
			id := firstNonEmpty(bucket.ID, bucket.Key)
			if id == "" || seen[service.ServiceID+"/"+id] {
				continue
			}
			items = append(items, manifestItem(service.ServiceID, id, "storage_bucket", service.Status, service.Status == serviceregistry.StatusEnabled, map[string]any{"description": bucket.Description}))
		}
	}
	return items
}

func collectRuntimeServiceDeclarations(snapshot Snapshot) ([]RuntimeService, []RuntimeService) {
	services := make([]RuntimeService, 0)
	workers := make([]RuntimeService, 0)
	seenServices := map[string]bool{}
	seenWorkers := map[string]bool{}
	routeMap := routesByService(snapshot.GatewayRoutes)

	addService := func(item RuntimeService, isWorker bool) {
		if item.ServiceID == "" {
			return
		}
		item.Routes = appendMissingStrings(item.Routes, routeMap[item.ServiceID]...)
		if isWorker {
			if seenWorkers[item.ServiceID] {
				return
			}
			seenWorkers[item.ServiceID] = true
			workers = append(workers, item)
			return
		}
		if seenServices[item.ServiceID] {
			return
		}
		seenServices[item.ServiceID] = true
		services = append(services, item)
	}

	for _, service := range snapshot.ServiceNodes {
		manifest, ok := decodeManifest(service.Manifest)
		if !ok {
			continue
		}
		for _, item := range manifest.Provides.Services {
			addService(runtimeServiceFromManifest(service.ServiceID, item, false), false)
		}
		for _, item := range manifest.Provides.Workers {
			addService(runtimeServiceFromManifest(service.ServiceID, item, true), true)
		}
	}
	for _, component := range snapshot.Components {
		switch component.Type {
		case "backend_service":
			addService(runtimeServiceFromComponent(component, routeMap[component.ComponentID], false), false)
		case "worker_service":
			addService(runtimeServiceFromComponent(component, nil, true), true)
		}
	}
	sortRuntimeServices(services)
	sortRuntimeServices(workers)
	return services, workers
}

func RebuildRuntimeTopology(snapshot Snapshot) ([]RuntimeTopologyNode, []RuntimeTopologyEdge) {
	return buildTopology(
		snapshot.ServiceNodes,
		mapServices(snapshot.ServiceNodes),
		snapshot.Components,
		snapshot.Services,
		snapshot.Workers,
		snapshot.GatewayRoutes,
		snapshot.Menus,
		snapshot.FrontendRoutes,
		snapshot.HealthChecks,
		snapshot.Topology.DependencyEdges,
	)
}

func runtimeServiceFromManifest(serviceID string, item manifestServiceItem, isWorker bool) RuntimeService {
	kind := strings.TrimSpace(item.Kind)
	if kind == "" {
		if isWorker {
			kind = "worker"
		} else {
			kind = "http"
		}
	}
	lifecycle := strings.ToLower(strings.TrimSpace(item.Lifecycle))
	if lifecycle == "" {
		lifecycle = LifecycleManaged
	}
	runtime := strings.ToLower(strings.TrimSpace(item.TrustedRuntime))
	if runtime == "" {
		if lifecycle == LifecycleMetadata {
			runtime = "metadata"
		} else {
			runtime = "compose"
		}
	}
	runtimeServiceID := strings.TrimSpace(item.ID)
	return RuntimeService{
		OwnerServiceID: serviceID,
		ServiceID:      runtimeServiceID,
		Name:           firstNonEmpty(item.Name, runtimeServiceID),
		Kind:           kind,
		Lifecycle:      lifecycle,
		Runtime:        runtime,
		ComposeService: strings.TrimSpace(item.ComposeService),
		State:          ServiceStateDeclared,
		Health:         "unknown",
		Required:       item.Required,
		Routes:         cleanStringList(item.Routes),
		HealthCheckID:  strings.TrimSpace(item.HealthCheckID),
		Status:         ServiceStateDeclared,
	}
}

func runtimeServiceFromComponent(component RuntimeComponent, routes []string, isWorker bool) RuntimeService {
	var cfg struct {
		Service        string   `json:"service"`
		Health         string   `json:"health"`
		Exposure       string   `json:"exposure"`
		Mode           string   `json:"mode"`
		Lifecycle      string   `json:"lifecycle"`
		TrustedRuntime string   `json:"trusted_runtime"`
		ComposeService string   `json:"compose_service"`
		HealthCheckID  string   `json:"health_check_id"`
		Routes         []string `json:"routes"`
		Required       bool     `json:"required"`
	}
	_ = json.Unmarshal(component.Config, &cfg)
	serviceID := firstNonEmpty(cfg.Service, component.ComponentID)
	kind := "http"
	if isWorker {
		kind = "worker"
	}
	lifecycle := strings.ToLower(strings.TrimSpace(cfg.Lifecycle))
	if lifecycle == "" {
		lifecycle = LifecycleManaged
	}
	runtime := strings.ToLower(strings.TrimSpace(cfg.TrustedRuntime))
	if runtime == "" {
		if lifecycle == LifecycleMetadata {
			runtime = "metadata"
		} else {
			runtime = "compose"
		}
	}
	return RuntimeService{
		OwnerServiceID: component.ServiceID,
		ServiceID:      serviceID,
		Name:           serviceID,
		Kind:           kind,
		Lifecycle:      lifecycle,
		Runtime:        runtime,
		ComposeService: firstNonEmpty(cfg.ComposeService, serviceID),
		State:          ServiceStateDeclared,
		Health:         "unknown",
		Required:       cfg.Required,
		Routes:         appendMissingStrings(cleanStringList(cfg.Routes), routes...),
		HealthCheckID:  strings.TrimSpace(cfg.HealthCheckID),
		Status:         component.Status,
	}
}

func routesByService(routes []serviceregistry.GatewayRoute) map[string][]string {
	out := map[string][]string{}
	for _, route := range routes {
		serviceID := strings.TrimSpace(route.TargetService)
		if serviceID == "" {
			continue
		}
		out[serviceID] = appendMissingStrings(out[serviceID], cleanPrefix(route.Prefix))
	}
	return out
}

func buildTopology(
	serviceNodes []serviceregistry.Service,
	serviceByID map[string]serviceregistry.Service,
	components []RuntimeComponent,
	services []RuntimeService,
	workers []RuntimeService,
	gatewayRoutes []serviceregistry.GatewayRoute,
	menus []serviceregistry.Menu,
	frontendRoutes []serviceregistry.FrontendRoute,
	healthChecks []RuntimeComponent,
	dependencyEdges []serviceregistry.Edge,
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
	for _, service := range services {
		addNode(RuntimeTopologyNode{
			ID:        service.ServiceID,
			ServiceID: service.ServiceID,
			Label:     firstNonEmpty(service.Name, service.ServiceID),
			Type:      "service",
			Status:    service.Status,
			Source:    "registry",
			Config:    json.RawMessage(`{}`),
		})
	}
	for _, edge := range dependencyEdges {
		edges = append(edges, RuntimeTopologyEdge{
			ID:        edge.FromServiceID + "->" + edge.ToServiceID + ":" + edge.EdgeType,
			ServiceID: edge.FromServiceID,
			From:      edge.FromServiceID,
			To:        edge.ToServiceID,
			Type:      edge.EdgeType,
			Required:  edge.Required,
			Source:    "registry",
		})
	}
	for _, component := range components {
		id := topologyID(component.ServiceID, "component", component.ComponentID)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: component.ServiceID,
			Label:     component.ComponentID,
			Type:      component.Type,
			Status:    component.Status,
			Source:    "registry",
			Config:    component.Config,
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:        component.ServiceID + "->" + id,
			ServiceID: component.ServiceID,
			From:      component.ServiceID,
			To:        id,
			Type:      "provides",
			Required:  false,
			Source:    "registry",
		})
	}
	for _, service := range services {
		id := topologyID(service.ServiceID, "service", service.ServiceID)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: service.ServiceID,
			Label:     firstNonEmpty(service.Name, service.ServiceID),
			Type:      "service",
			Status:    service.State,
			Source:    "runtime",
			Config:    mustRaw(map[string]any{"service_id": service.ServiceID, "runtime": service.Runtime, "lifecycle": service.Lifecycle, "health": service.Health, "routes": service.Routes}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:        service.ServiceID + "->" + id + ":runtime-service",
			ServiceID: service.ServiceID,
			From:      service.ServiceID,
			To:        id,
			Type:      "runtime_service",
			Required:  service.Required,
			Source:    "runtime",
		})
		if service.HealthCheckID != "" {
			edges = append(edges, RuntimeTopologyEdge{
				ID:        id + "->" + topologyID(service.ServiceID, "health", service.HealthCheckID),
				ServiceID: service.ServiceID,
				From:      id,
				To:        topologyID(service.ServiceID, "health", service.HealthCheckID),
				Type:      "health",
				Required:  service.Required,
				Source:    "runtime",
			})
		}
	}
	for _, worker := range workers {
		id := topologyID(worker.ServiceID, "worker", worker.ServiceID)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: worker.ServiceID,
			Label:     firstNonEmpty(worker.Name, worker.ServiceID),
			Type:      "worker",
			Status:    worker.State,
			Source:    "runtime",
			Config:    mustRaw(map[string]any{"service_id": worker.ServiceID, "runtime": worker.Runtime, "lifecycle": worker.Lifecycle, "health": worker.Health}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:        worker.ServiceID + "->" + id + ":runtime-worker",
			ServiceID: worker.ServiceID,
			From:      worker.ServiceID,
			To:        id,
			Type:      "runtime_worker",
			Required:  worker.Required,
			Source:    "runtime",
		})
		if worker.HealthCheckID != "" {
			edges = append(edges, RuntimeTopologyEdge{
				ID:        id + "->" + topologyID(worker.ServiceID, "health", worker.HealthCheckID),
				ServiceID: worker.ServiceID,
				From:      id,
				To:        topologyID(worker.ServiceID, "health", worker.HealthCheckID),
				Type:      "health",
				Required:  worker.Required,
				Source:    "runtime",
			})
		}
	}
	for _, route := range gatewayRoutes {
		id := topologyID(route.ServiceID, "gateway_route", route.Prefix)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: route.ServiceID,
			Label:     route.Prefix,
			Type:      "gateway_route",
			Status:    boolStatus(route.Enabled),
			Source:    "registry",
			Config:    mustRaw(map[string]any{"auth_mode": route.AuthMode, "target_service": route.TargetService}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:        route.ServiceID + "->" + id,
			ServiceID: route.ServiceID,
			From:      route.ServiceID,
			To:        id,
			Type:      "routes",
			Required:  false,
			Source:    "registry",
		})
	}
	for _, menu := range menus {
		id := topologyID(menu.ServiceID, "menu", menu.MenuKey)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: menu.ServiceID,
			Label:     menu.Title,
			Type:      "menu",
			Status:    boolStatus(menu.Enabled),
			Source:    "registry",
			Config:    mustRaw(map[string]any{"route_path": menu.RoutePath, "required_permission": menu.RequiredPermission}),
		})
		edges = append(edges, RuntimeTopologyEdge{
			ID:        menu.ServiceID + "->" + id,
			ServiceID: menu.ServiceID,
			From:      menu.ServiceID,
			To:        id,
			Type:      "menu",
			Required:  false,
			Source:    "registry",
		})
	}
	for _, route := range frontendRoutes {
		id := topologyID(route.ServiceID, "frontend_route", route.RoutePath)
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: route.ServiceID,
			Label:     firstNonEmpty(route.RouteName, route.RoutePath),
			Type:      "frontend_route",
			Status:    boolStatus(route.Enabled),
			Source:    "registry",
			Config:    mustRaw(map[string]any{"route_path": route.RoutePath, "component_key": route.ComponentKey, "required_permission": route.RequiredPermission}),
		})
	}
	for _, health := range healthChecks {
		id := topologyID(health.ServiceID, "health", health.ComponentID)
		if knownNodes[id] {
			continue
		}
		addNode(RuntimeTopologyNode{
			ID:        id,
			ServiceID: health.ServiceID,
			Label:     health.ComponentID,
			Type:      "health_check",
			Status:    health.Status,
			Source:    "registry",
			Config:    health.Config,
		})
	}
	for _, service := range serviceNodes {
		manifest, ok := decodeManifest(service.Manifest)
		if !ok {
			continue
		}
		for _, item := range manifest.Provides.Topology.Nodes {
			id := topologyID(service.ServiceID, "manifest", item.ID)
			addNode(RuntimeTopologyNode{
				ID:        id,
				ServiceID: service.ServiceID,
				Label:     firstNonEmpty(item.Label, item.ID),
				Type:      item.Type,
				Status:    service.Status,
				Source:    "manifest",
				Config:    json.RawMessage(`{}`),
			})
			if serviceByID[service.ServiceID].ServiceID != "" {
				edges = append(edges, RuntimeTopologyEdge{
					ID:        service.ServiceID + "->" + id + ":declares",
					ServiceID: service.ServiceID,
					From:      service.ServiceID,
					To:        id,
					Type:      "declares",
					Required:  false,
					Source:    "manifest",
				})
			}
		}
		for _, edge := range manifest.Provides.Topology.Edges {
			edges = append(edges, RuntimeTopologyEdge{
				ID:        topologyID(service.ServiceID, "manifest_edge", edge.From+"->"+edge.To+":"+edge.Type),
				ServiceID: service.ServiceID,
				From:      topologyID(service.ServiceID, "manifest", edge.From),
				To:        topologyID(service.ServiceID, "manifest", edge.To),
				Type:      edge.Type,
				Required:  false,
				Source:    "manifest",
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

func manifestItem(serviceID string, id string, typ string, status string, enabled bool, config map[string]any) RuntimeManifestItem {
	return RuntimeManifestItem{
		ServiceID: serviceID,
		ID:        id,
		Type:      typ,
		Status:    status,
		Enabled:   enabled,
		Config:    mustRaw(config),
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
		return serviceregistry.StatusEnabled
	}
	return "DISABLED"
}

func cleanStringList(items []string) []string {
	out := make([]string, 0, len(items))
	for _, item := range items {
		value := strings.TrimSpace(item)
		if value == "" {
			continue
		}
		out = appendMissingStrings(out, value)
	}
	return out
}

func appendMissingStrings(items []string, values ...string) []string {
	seen := map[string]bool{}
	for _, item := range items {
		if item != "" {
			seen[item] = true
		}
	}
	for _, value := range values {
		value = strings.TrimSpace(value)
		if value == "" || seen[value] {
			continue
		}
		seen[value] = true
		items = append(items, value)
	}
	return items
}

func topologyID(serviceID string, kind string, value string) string {
	cleaned := strings.NewReplacer("/", "_", ":", "_", " ", "_").Replace(strings.TrimSpace(value))
	return serviceID + ":" + kind + ":" + cleaned
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

func defaultRouteMethods() []string {
	return []string{"GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"}
}

func routeID(serviceID string, prefix string) string {
	return serviceID + ":" + cleanPrefix(prefix)
}

func normalizeTrustedServices(items map[string]TrustedService) map[string]TrustedService {
	out := make(map[string]TrustedService, len(items))
	for key, item := range items {
		serviceID := strings.TrimSpace(firstNonEmpty(item.ServiceID, key))
		if serviceID == "" {
			continue
		}
		item.ServiceID = serviceID
		item.UpstreamBase = strings.TrimRight(strings.TrimSpace(item.UpstreamBase), "/")
		item.StripPrefix = cleanPrefix(item.StripPrefix)
		item.RewritePrefix = cleanPrefix(item.RewritePrefix)
		out[serviceID] = item
	}
	return out
}

func normalizeReservedPrefixes(items []string) []string {
	if len(items) == 0 {
		items = DefaultReservedPrefixes()
	}
	out := make([]string, 0, len(items))
	seen := map[string]bool{}
	for _, item := range items {
		prefix := cleanPrefix(item)
		if prefix == "" || seen[prefix] {
			continue
		}
		seen[prefix] = true
		out = append(out, prefix)
	}
	sort.Slice(out, func(i, j int) bool {
		if len(out[i]) == len(out[j]) {
			return out[i] < out[j]
		}
		return len(out[i]) > len(out[j])
	})
	return out
}

func DefaultReservedPrefixes() []string {
	return []string{
		"/api/auth",
		"/api/admin/services",
		"/api/admin/health",
		"/api/health",
		"/api/internal",
		"/api/judge/worker",
	}
}

func reservedPrefixMatches(prefix string, reserved []string) bool {
	for _, item := range reserved {
		if prefix == item || strings.HasPrefix(prefix, item+"/") || strings.HasPrefix(item, prefix+"/") {
			return true
		}
	}
	return false
}

func hasDuplicateConflict(items []string) bool {
	for _, item := range items {
		if strings.Contains(item, "duplicate prefix") {
			return true
		}
	}
	return false
}

func sortSnapshot(snapshot *Snapshot) {
	sort.Slice(snapshot.ServiceNodes, func(i, j int) bool { return snapshot.ServiceNodes[i].ServiceID < snapshot.ServiceNodes[j].ServiceID })
	sort.Slice(snapshot.Permissions, func(i, j int) bool {
		if snapshot.Permissions[i].ServiceID == snapshot.Permissions[j].ServiceID {
			return snapshot.Permissions[i].PermissionKey < snapshot.Permissions[j].PermissionKey
		}
		return snapshot.Permissions[i].ServiceID < snapshot.Permissions[j].ServiceID
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
		if snapshot.Components[i].ServiceID == snapshot.Components[j].ServiceID {
			return snapshot.Components[i].ComponentID < snapshot.Components[j].ComponentID
		}
		return snapshot.Components[i].ServiceID < snapshot.Components[j].ServiceID
	})
	sortRuntimeServices(snapshot.Services)
	sortRuntimeServices(snapshot.Workers)
	sort.Slice(snapshot.HealthChecks, func(i, j int) bool {
		return snapshot.HealthChecks[i].ComponentID < snapshot.HealthChecks[j].ComponentID
	})
	sort.Slice(snapshot.StorageBuckets, func(i, j int) bool { return manifestLess(snapshot.StorageBuckets[i], snapshot.StorageBuckets[j]) })
	sort.Slice(snapshot.Operations, func(i, j int) bool { return manifestLess(snapshot.Operations[i], snapshot.Operations[j]) })
	sort.Slice(snapshot.Topology.Nodes, func(i, j int) bool { return snapshot.Topology.Nodes[i].ID < snapshot.Topology.Nodes[j].ID })
	sort.Slice(snapshot.Topology.Edges, func(i, j int) bool { return snapshot.Topology.Edges[i].ID < snapshot.Topology.Edges[j].ID })
	sort.Slice(snapshot.Topology.ServiceNodes, func(i, j int) bool {
		return snapshot.Topology.ServiceNodes[i].ServiceID < snapshot.Topology.ServiceNodes[j].ServiceID
	})
	sort.Slice(snapshot.Topology.DependencyEdges, func(i, j int) bool {
		if snapshot.Topology.DependencyEdges[i].FromServiceID == snapshot.Topology.DependencyEdges[j].FromServiceID {
			return snapshot.Topology.DependencyEdges[i].ToServiceID < snapshot.Topology.DependencyEdges[j].ToServiceID
		}
		return snapshot.Topology.DependencyEdges[i].FromServiceID < snapshot.Topology.DependencyEdges[j].FromServiceID
	})
	sort.Strings(snapshot.Warnings)
}

func manifestLess(left RuntimeManifestItem, right RuntimeManifestItem) bool {
	if left.ServiceID == right.ServiceID {
		return left.ID < right.ID
	}
	return left.ServiceID < right.ServiceID
}

func sortRouteTable(table *RouteTable) {
	sort.Slice(table.Routes, func(i, j int) bool {
		if table.Routes[i].Priority == table.Routes[j].Priority {
			return table.Routes[i].Prefix < table.Routes[j].Prefix
		}
		return table.Routes[i].Priority > table.Routes[j].Priority
	})
	for i := range table.Routes {
		sort.Strings(table.Routes[i].Conflicts)
		sort.Strings(table.Routes[i].Warnings)
		sort.Strings(table.Routes[i].BlockedBy)
	}
	sort.Strings(table.Warnings)
}
