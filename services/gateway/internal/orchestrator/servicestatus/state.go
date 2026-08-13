package servicestatus

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
)

type SnapshotReader interface {
	ListServices(context.Context) ([]orchestratorsnapshot.Service, error)
	ListPermissions(context.Context) ([]orchestratorsnapshot.Permission, error)
	ListMenus(context.Context) ([]orchestratorsnapshot.Menu, error)
	ListFrontendRoutes(context.Context) ([]orchestratorsnapshot.FrontendRoute, error)
	ListGatewayRoutes(context.Context) ([]orchestratorsnapshot.GatewayRoute, error)
	ListComponents(context.Context) ([]orchestratorsnapshot.Component, error)
	ListEdges(context.Context) ([]orchestratorsnapshot.Edge, error)
}

type Snapshot struct {
	Version            string                               `json:"version"`
	GeneratedAt        string                               `json:"generated_at"`
	ServiceDefinitions []orchestratorsnapshot.Service       `json:"service_definitions"`
	Permissions        []orchestratorsnapshot.Permission    `json:"permissions"`
	Roles              []ServiceManifestItem                `json:"roles"`
	Menus              []orchestratorsnapshot.Menu          `json:"menus"`
	FrontendRoutes     []orchestratorsnapshot.FrontendRoute `json:"frontend_routes"`
	GatewayRoutes      []orchestratorsnapshot.GatewayRoute  `json:"gateway_routes"`
	Components         []ServiceComponent                   `json:"components"`
	Services           []ServiceStatus                      `json:"services"`
	Workers            []ServiceStatus                      `json:"workers"`
	StorageBuckets     []ServiceManifestItem                `json:"storage_buckets"`
	HealthChecks       []ServiceComponent                   `json:"health_checks"`
	Operations         []ServiceManifestItem                `json:"operations"`
	Topology           ServiceTopology                      `json:"topology"`
	Warnings           []string                             `json:"warnings"`
}

type BuildOptions struct {
	IncludeDisabled bool
}

type ServiceComponent struct {
	ServiceID   string          `json:"service_id"`
	ComponentID string          `json:"component_id"`
	Type        string          `json:"type"`
	Status      string          `json:"status"`
	Config      json.RawMessage `json:"config"`
}

type ServiceManifestItem struct {
	ServiceID string          `json:"service_id"`
	ID        string          `json:"id"`
	Type      string          `json:"type"`
	Status    string          `json:"status"`
	Enabled   bool            `json:"enabled"`
	Config    json.RawMessage `json:"config"`
}

type ServiceTopology struct {
	Nodes              []ServiceTopologyNode          `json:"nodes"`
	Edges              []ServiceTopologyEdge          `json:"edges"`
	DependencyEdges    []orchestratorsnapshot.Edge    `json:"dependency_edges"`
	ServiceDefinitions []orchestratorsnapshot.Service `json:"service_definitions"`
}

type ServiceTopologyNode struct {
	ID        string          `json:"id"`
	ServiceID string          `json:"service_id"`
	Label     string          `json:"label"`
	Type      string          `json:"type"`
	Status    string          `json:"status"`
	Source    string          `json:"source"`
	Config    json.RawMessage `json:"config"`
}

type ServiceTopologyEdge struct {
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
	Routes      []ServiceRoute `json:"routes"`
	Warnings    []string       `json:"warnings"`
	CanProxy    bool           `json:"can_proxy"`
}

type ServiceRoute struct {
	RouteID              string           `json:"route_id"`
	ApiID                string           `json:"api_id,omitempty"`
	OperationID          string           `json:"operation_id,omitempty"`
	DeploymentID         string           `json:"deployment_id,omitempty"`
	RevisionID           string           `json:"revision_id,omitempty"`
	Generation           uint64           `json:"generation,omitempty"`
	Audience             string           `json:"audience,omitempty"`
	PathTemplate         string           `json:"path_template,omitempty"`
	ProviderPath         string           `json:"provider_path,omitempty"`
	BindingID            string           `json:"binding_id,omitempty"`
	ConsumerDeploymentID string           `json:"consumer_deployment_id,omitempty"`
	ConsumerServiceID    string           `json:"consumer_service_id,omitempty"`
	ConsumerNodeID       string           `json:"consumer_node_id,omitempty"`
	CredentialGeneration uint64           `json:"credential_generation,omitempty"`
	TimeoutMS            uint64           `json:"timeout_ms,omitempty"`
	NodeID               string           `json:"node_id,omitempty"`
	ProviderNodeID       string           `json:"provider_node_id,omitempty"`
	ProviderHostIP       string           `json:"provider_host_ip,omitempty"`
	ProviderService      string           `json:"provider_service_name,omitempty"`
	ProviderEndpoint     string           `json:"provider_endpoint,omitempty"`
	VisibilitySource     string           `json:"visibility_source,omitempty"`
	Distance             int              `json:"distance,omitempty"`
	OwnerServiceID       string           `json:"owner_service_id"`
	Prefix               string           `json:"prefix"`
	ServiceID            string           `json:"service_id"`
	TargetService        string           `json:"target_service"`
	UpstreamBase         string           `json:"upstream_base,omitempty"`
	AuthMode             string           `json:"auth_mode"`
	ProviderAuthMode     string           `json:"provider_auth_mode,omitempty"`
	RequiredPermission   string           `json:"required_permission,omitempty"`
	PermissionScope      *PermissionScope `json:"permission_scope,omitempty"`
	Methods              []string         `json:"methods"`
	Enabled              bool             `json:"enabled"`
	ProxyEnabled         bool             `json:"proxy_enabled"`
	Priority             int              `json:"priority"`
	StripPrefix          string           `json:"strip_prefix,omitempty"`
	RewritePrefix        string           `json:"rewrite_prefix,omitempty"`
	HealthCheckID        string           `json:"health_check_id,omitempty"`
	CreatedFrom          string           `json:"created_from"`
	Status               string           `json:"status"`
	ServiceStatus        string           `json:"service_status,omitempty"`
	ServiceHealth        string           `json:"service_health,omitempty"`
	Conflicts            []string         `json:"conflicts"`
	Warnings             []string         `json:"warnings"`
	BlockedBy            []string         `json:"blocked_by"`
}

type PermissionScope struct {
	Kind          string `json:"kind"`
	Type          string `json:"type"`
	PathParameter string `json:"path_parameter,omitempty"`
}

func ContributionRouteTable(snapshot orchestratorsnapshot.ContributionSnapshot) (RouteTable, error) {
	table := RouteTable{
		Version:  strings.TrimSpace(snapshot.Digest),
		CanProxy: false,
	}
	seen := make(map[string]int)
	reserved := normalizeReservedPrefixes(nil)
	for _, route := range snapshot.GatewayRoutes {
		method := strings.ToUpper(strings.TrimSpace(route.Method))
		path := cleanPrefix(route.Path)
		audience := strings.ToLower(strings.TrimSpace(route.Audience))
		item := ServiceRoute{
			RouteID:            contributionRouteID(route),
			ApiID:              strings.TrimSpace(route.ApiID),
			OperationID:        strings.TrimSpace(route.OperationID),
			DeploymentID:       strings.TrimSpace(route.DeploymentID),
			RevisionID:         strings.TrimSpace(route.RevisionID),
			Generation:         route.Generation,
			Audience:           audience,
			PathTemplate:       path,
			ProviderPath:       cleanPrefix(route.ProviderPath),
			OwnerServiceID:     strings.TrimSpace(route.ServiceID),
			Prefix:             path,
			ServiceID:          strings.TrimSpace(route.ServiceID),
			TargetService:      strings.TrimSpace(route.ServiceID),
			UpstreamBase:       strings.TrimRight(strings.TrimSpace(route.UpstreamBase), "/"),
			AuthMode:           normalizeContributionAuth(route.Auth, audience),
			RequiredPermission: normalizeRequiredPermission(route.Permission),
			PermissionScope:    contributionPermissionScope(route),
			Methods:            []string{method},
			Enabled:            route.Enabled,
			Priority:           templateSpecificity(path),
			CreatedFrom:        "contribution_snapshot_v1",
			Status:             "active",
		}
		if !item.Enabled {
			item.Status = "disabled"
			item.BlockedBy = append(item.BlockedBy, "runtime not ready")
		}
		if item.ServiceID == "" || item.DeploymentID == "" || item.RevisionID == "" || item.ApiID == "" || item.OperationID == "" {
			item.BlockedBy = append(item.BlockedBy, "incomplete operation identity")
		}
		if strings.EqualFold(item.ServiceID, "gateway") {
			item.BlockedBy = append(item.BlockedBy, "Gateway platform service cannot contribute proxy routes")
		}
		shape, validTemplate := contributionTemplateShape(path)
		providerParameters, validProviderTemplate := contributionTemplateParameters(item.ProviderPath)
		pathParameters, _ := contributionTemplateParameters(path)
		if method == "" || path == "" || item.ProviderPath == "" || !validContributionAudience(audience) || !validTemplate || !validProviderTemplate || !sameStringSet(pathParameters, providerParameters) {
			item.BlockedBy = append(item.BlockedBy, "invalid operation route")
		}
		if item.RequiredPermission == "" && item.PermissionScope != nil {
			item.BlockedBy = append(item.BlockedBy, "permission scope without permission")
		}
		if item.RequiredPermission != "" && !validPermissionScope(item.PermissionScope, pathParameters, providerParameters) {
			item.BlockedBy = append(item.BlockedBy, "invalid permission scope")
		}
		if item.UpstreamBase == "" {
			item.BlockedBy = append(item.BlockedBy, "runtime upstream unavailable")
		}
		if reservedPrefixMatches(path, reserved) {
			item.BlockedBy = append(item.BlockedBy, "reserved prefix")
		}
		methods := []string{method}
		if method == "GET" {
			methods = append(methods, "HEAD")
		}
		for _, matchingMethod := range methods {
			key := audience + "\x00" + matchingMethod + "\x00" + shape
			if previous, exists := seen[key]; exists {
				return RouteTable{}, fmt.Errorf("contribution routes %s and %s collide for %s %s %s", table.Routes[previous].RouteID, item.RouteID, audience, matchingMethod, path)
			}
			seen[key] = len(table.Routes)
		}
		if len(item.BlockedBy) > 0 {
			item.Status = "blocked"
		}
		table.Routes = append(table.Routes, item)
	}
	for i := range table.Routes {
		table.Routes[i].ProxyEnabled = table.Routes[i].Enabled && table.Routes[i].Status == "active" && len(table.Routes[i].BlockedBy) == 0
		if table.Routes[i].ProxyEnabled {
			table.CanProxy = true
		}
		if len(table.Routes[i].BlockedBy) > 0 {
			table.Warnings = append(table.Warnings, table.Routes[i].RouteID+": "+strings.Join(table.Routes[i].BlockedBy, "; "))
		}
	}
	sortRouteTable(&table)
	return table, nil
}

func contributionPermissionScope(route orchestratorsnapshot.ContributionGatewayRoute) *PermissionScope {
	if strings.TrimSpace(route.Permission) == "" {
		if route.PermissionScope == nil {
			return nil
		}
		return &PermissionScope{}
	}
	if route.PermissionScope == nil || route.PermissionScope.Kind == "system" {
		return &PermissionScope{Kind: "system", Type: "system"}
	}
	return &PermissionScope{
		Kind:          strings.TrimSpace(route.PermissionScope.Kind),
		Type:          strings.TrimSpace(route.PermissionScope.Type),
		PathParameter: strings.TrimSpace(route.PermissionScope.PathParameter),
	}
}

func validPermissionScope(scope *PermissionScope, externalParameters, providerParameters map[string]bool) bool {
	if scope == nil {
		return false
	}
	if scope.Kind == "system" {
		return scope.Type == "system" && scope.PathParameter == ""
	}
	if scope.Kind != "path_parameter" || scope.Type == "" || scope.Type == "system" || scope.PathParameter == "" {
		return false
	}
	return externalParameters[scope.PathParameter] && providerParameters[scope.PathParameter]
}

func contributionRouteID(route orchestratorsnapshot.ContributionGatewayRoute) string {
	return strings.Join([]string{
		"contribution", strings.TrimSpace(route.ServiceID), strings.TrimSpace(route.DeploymentID),
		strings.TrimSpace(route.RevisionID), strings.TrimSpace(route.OperationID),
	}, ":")
}

func normalizeContributionAuth(auth string, audience string) string {
	switch strings.ToLower(strings.TrimSpace(auth)) {
	case "anonymous":
		return "public"
	case "optional":
		return "optional"
	case "required":
		if strings.EqualFold(audience, "admin") {
			return "admin"
		}
		return "user"
	default:
		return strings.ToLower(strings.TrimSpace(auth))
	}
}

func validContributionAudience(audience string) bool {
	switch audience {
	case "public", "user", "admin", "internal":
		return true
	default:
		return false
	}
}

func templateSpecificity(path string) int {
	literal := 0
	for _, segment := range strings.Split(strings.Trim(path, "/"), "/") {
		if !strings.HasPrefix(segment, "{") || !strings.HasSuffix(segment, "}") {
			literal++
		}
	}
	return literal*1000 + len(path)
}

func contributionTemplateShape(path string) (string, bool) {
	segments := strings.Split(strings.Trim(cleanPrefix(path), "/"), "/")
	if len(segments) == 1 && segments[0] == "" {
		return "/", true
	}
	for index, segment := range segments {
		if segment == "" || segment == "." || segment == ".." {
			return "", false
		}
		if strings.HasPrefix(segment, "{") && strings.HasSuffix(segment, "}") && len(segment) > 2 {
			segments[index] = "{}"
		} else if strings.ContainsAny(segment, "{}") {
			return "", false
		}
	}
	return "/" + strings.Join(segments, "/"), true
}

func contributionTemplateParameters(path string) (map[string]bool, bool) {
	parameters := make(map[string]bool)
	segments := strings.Split(strings.Trim(cleanPrefix(path), "/"), "/")
	for _, segment := range segments {
		if strings.HasPrefix(segment, "{") && strings.HasSuffix(segment, "}") && len(segment) > 2 {
			name := strings.TrimSpace(segment[1 : len(segment)-1])
			if name == "" || parameters[name] {
				return nil, false
			}
			parameters[name] = true
		} else if strings.ContainsAny(segment, "{}") {
			return nil, false
		}
	}
	return parameters, true
}

func sameStringSet(left map[string]bool, right map[string]bool) bool {
	if len(left) != len(right) {
		return false
	}
	for item := range left {
		if !right[item] {
			return false
		}
	}
	return true
}

type RouteTableOptions struct {
	TrustedServices       map[string]TrustedService
	ServiceStatuses       map[string]ServiceStatus
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

func BuildSnapshot(ctx context.Context, reader SnapshotReader) (Snapshot, error) {
	return BuildSnapshotWithOptions(ctx, reader, BuildOptions{})
}

func BuildSnapshotWithOptions(ctx context.Context, reader SnapshotReader, opts BuildOptions) (Snapshot, error) {
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
		Version:            "1",
		GeneratedAt:        time.Now().UTC().Format(time.RFC3339Nano),
		ServiceDefinitions: filterServices(services, visibleServices),
		Permissions:        filterPermissions(permissions, visibleServices),
		Menus:              filterMenus(menus, visibleServices),
		FrontendRoutes:     filterFrontendRoutes(frontendRoutes, visibleServices),
		GatewayRoutes:      filterGatewayRoutes(gatewayRoutes, visibleServices),
		Topology: ServiceTopology{
			ServiceDefinitions: filterServices(services, visibleServices),
			DependencyEdges:    filterEdges(edges, visibleServices),
		},
	}
	for _, component := range components {
		if !visibleServices[component.ServiceID] {
			continue
		}
		item := ServiceComponent{
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
	snapshot.Roles = manifestItemsFromServices(snapshot.ServiceDefinitions, "roles")
	snapshot.StorageBuckets = storageBucketItems(snapshot.ServiceDefinitions, snapshot.Components)
	snapshot.Operations = manifestItemsFromServices(snapshot.ServiceDefinitions, "operations")
	snapshot.Services, snapshot.Workers = collectServiceStatusDeclarations(snapshot)
	snapshot.Topology.Nodes, snapshot.Topology.Edges = buildTopology(snapshot.ServiceDefinitions, serviceByID, snapshot.Components, snapshot.Services, snapshot.Workers, snapshot.GatewayRoutes, snapshot.Menus, snapshot.FrontendRoutes, snapshot.HealthChecks, snapshot.Topology.DependencyEdges)
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
		serviceStatus, hasServiceStatus := opts.ServiceStatuses[serviceID]
		item := ServiceRoute{
			RouteID:              routeID(route.ServiceID, route.Prefix),
			ApiID:                strings.TrimSpace(route.ApiID),
			BindingID:            strings.TrimSpace(route.BindingID),
			ConsumerDeploymentID: strings.TrimSpace(route.ConsumerDeploymentID),
			CredentialGeneration: route.CredentialGeneration,
			TimeoutMS:            route.TimeoutMS,
			NodeID:               strings.TrimSpace(route.NodeID),
			ProviderNodeID:       strings.TrimSpace(route.ProviderNodeID),
			ProviderHostIP:       strings.TrimSpace(route.ProviderHostIP),
			ProviderService:      strings.TrimSpace(route.ProviderService),
			ProviderEndpoint:     strings.TrimSpace(route.ProviderEndpoint),
			VisibilitySource:     strings.TrimSpace(route.VisibilitySource),
			Distance:             route.Distance,
			OwnerServiceID:       route.ServiceID,
			Prefix:               cleanPrefix(route.Prefix),
			ServiceID:            serviceID,
			TargetService:        serviceID,
			UpstreamBase:         strings.TrimRight(strings.TrimSpace(route.UpstreamBase), "/"),
			AuthMode:             normalizeRouteAuthMode(route.AuthMode),
			RequiredPermission:   normalizeRequiredPermission(route.RequiredPermission),
			Methods:              defaultRouteMethods(),
			Enabled:              route.Enabled,
			Priority:             len(cleanPrefix(route.Prefix)),
			StripPrefix:          cleanPrefix(route.StripPrefix),
			RewritePrefix:        cleanPrefix(route.RewritePrefix),
			HealthCheckID:        strings.TrimSpace(route.HealthCheckID),
			CreatedFrom:          "orchestrator_snapshot",
			Status:               "active",
		}
		if serviceTrusted && item.UpstreamBase == "" {
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
		hasUpstream := item.UpstreamBase != ""
		if !serviceTrusted && !hasUpstream {
			item.BlockedBy = append(item.BlockedBy, "unknown trusted service")
		}
		if reservedPrefixMatches(item.Prefix, reserved) {
			item.BlockedBy = append(item.BlockedBy, "reserved prefix")
		}
		structuralBlocked := len(item.BlockedBy) > 0
		if hasServiceStatus {
			item.ServiceStatus = serviceStatus.State
			item.ServiceHealth = serviceStatus.Health
			item.HealthCheckID = firstNonEmpty(item.HealthCheckID, serviceStatus.HealthCheckID)
			switch serviceStatus.State {
			case ServiceStatusRunning:
			case ServiceStatusDegraded:
				item.Status = "degraded"
				item.BlockedBy = append(item.BlockedBy, "service degraded")
				item.Warnings = append(item.Warnings, "service health is "+serviceStatus.Health)
			case ServiceStatusDeclared, ServiceStatusInstalled, ServiceStatusEnabled, ServiceStatusStarting:
				item.Status = "degraded"
				item.BlockedBy = append(item.BlockedBy, "service not running")
				item.Warnings = append(item.Warnings, "service status is "+serviceStatus.State)
			default:
				item.Status = "unavailable"
				item.BlockedBy = append(item.BlockedBy, "service not running")
				item.Warnings = append(item.Warnings, "service status is "+serviceStatus.State)
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

func serviceVisibility(items []orchestratorsnapshot.Service, includeDisabled bool) map[string]bool {
	visible := make(map[string]bool, len(items))
	for _, item := range items {
		if includeDisabled || item.Status == orchestratorsnapshot.StatusEnabled {
			visible[item.ServiceID] = true
		}
	}
	return visible
}

func mapServices(items []orchestratorsnapshot.Service) map[string]orchestratorsnapshot.Service {
	out := make(map[string]orchestratorsnapshot.Service, len(items))
	for _, item := range items {
		out[item.ServiceID] = item
	}
	return out
}

func filterServices(items []orchestratorsnapshot.Service, visible map[string]bool) []orchestratorsnapshot.Service {
	out := make([]orchestratorsnapshot.Service, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterPermissions(items []orchestratorsnapshot.Permission, visible map[string]bool) []orchestratorsnapshot.Permission {
	out := make([]orchestratorsnapshot.Permission, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterMenus(items []orchestratorsnapshot.Menu, visible map[string]bool) []orchestratorsnapshot.Menu {
	out := make([]orchestratorsnapshot.Menu, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterFrontendRoutes(items []orchestratorsnapshot.FrontendRoute, visible map[string]bool) []orchestratorsnapshot.FrontendRoute {
	out := make([]orchestratorsnapshot.FrontendRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterGatewayRoutes(items []orchestratorsnapshot.GatewayRoute, visible map[string]bool) []orchestratorsnapshot.GatewayRoute {
	out := make([]orchestratorsnapshot.GatewayRoute, 0, len(items))
	for _, item := range items {
		if visible[item.ServiceID] {
			out = append(out, item)
		}
	}
	return out
}

func filterEdges(items []orchestratorsnapshot.Edge, visible map[string]bool) []orchestratorsnapshot.Edge {
	out := make([]orchestratorsnapshot.Edge, 0, len(items))
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

func manifestItemsFromServices(services []orchestratorsnapshot.Service, kind string) []ServiceManifestItem {
	items := make([]ServiceManifestItem, 0)
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

func storageBucketItems(services []orchestratorsnapshot.Service, components []ServiceComponent) []ServiceManifestItem {
	items := make([]ServiceManifestItem, 0)
	seen := map[string]bool{}
	for _, component := range components {
		if component.Type != "storage_bucket" {
			continue
		}
		key := component.ServiceID + "/" + component.ComponentID
		seen[key] = true
		items = append(items, ServiceManifestItem{
			ServiceID: component.ServiceID,
			ID:        component.ComponentID,
			Type:      "storage_bucket",
			Status:    component.Status,
			Enabled:   component.Status == orchestratorsnapshot.StatusEnabled,
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
			items = append(items, manifestItem(service.ServiceID, id, "storage_bucket", service.Status, service.Status == orchestratorsnapshot.StatusEnabled, map[string]any{"description": bucket.Description}))
		}
	}
	return items
}

func collectServiceStatusDeclarations(snapshot Snapshot) ([]ServiceStatus, []ServiceStatus) {
	services := make([]ServiceStatus, 0)
	workers := make([]ServiceStatus, 0)
	seenServices := map[string]bool{}
	seenWorkers := map[string]bool{}
	routeMap := routesByService(snapshot.GatewayRoutes)

	addService := func(item ServiceStatus, isWorker bool) {
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

	for _, service := range snapshot.ServiceDefinitions {
		manifest, ok := decodeManifest(service.Manifest)
		if !ok {
			continue
		}
		for _, item := range manifest.Provides.Services {
			addService(ServiceStatusFromManifest(service.ServiceID, item, false), false)
		}
		for _, item := range manifest.Provides.Workers {
			addService(ServiceStatusFromManifest(service.ServiceID, item, true), true)
		}
	}
	for _, component := range snapshot.Components {
		switch component.Type {
		case "backend_service":
			addService(ServiceStatusFromComponent(component, routeMap[component.ComponentID], false), false)
		case "worker_service":
			addService(ServiceStatusFromComponent(component, nil, true), true)
		}
	}
	sortServiceStatuses(services)
	sortServiceStatuses(workers)
	return services, workers
}

func RebuildServiceTopology(snapshot Snapshot) ([]ServiceTopologyNode, []ServiceTopologyEdge) {
	return buildTopology(
		snapshot.ServiceDefinitions,
		mapServices(snapshot.ServiceDefinitions),
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

func ServiceStatusFromManifest(serviceID string, item manifestServiceItem, isWorker bool) ServiceStatus {
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
	return ServiceStatus{
		OwnerServiceID: serviceID,
		ServiceID:      runtimeServiceID,
		Name:           firstNonEmpty(item.Name, runtimeServiceID),
		Kind:           kind,
		Lifecycle:      lifecycle,
		Runtime:        runtime,
		ComposeService: strings.TrimSpace(item.ComposeService),
		State:          ServiceStatusDeclared,
		Health:         "unknown",
		Required:       item.Required,
		Routes:         cleanStringList(item.Routes),
		HealthCheckID:  strings.TrimSpace(item.HealthCheckID),
		Status:         ServiceStatusDeclared,
	}
}

func ServiceStatusFromComponent(component ServiceComponent, routes []string, isWorker bool) ServiceStatus {
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
	return ServiceStatus{
		OwnerServiceID: component.ServiceID,
		ServiceID:      serviceID,
		Name:           serviceID,
		Kind:           kind,
		Lifecycle:      lifecycle,
		Runtime:        runtime,
		ComposeService: firstNonEmpty(cfg.ComposeService, serviceID),
		State:          ServiceStatusDeclared,
		Health:         "unknown",
		Required:       cfg.Required,
		Routes:         appendMissingStrings(cleanStringList(cfg.Routes), routes...),
		HealthCheckID:  strings.TrimSpace(cfg.HealthCheckID),
		Status:         component.Status,
	}
}

func routesByService(routes []orchestratorsnapshot.GatewayRoute) map[string][]string {
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
	serviceNodes []orchestratorsnapshot.Service,
	serviceByID map[string]orchestratorsnapshot.Service,
	components []ServiceComponent,
	services []ServiceStatus,
	workers []ServiceStatus,
	gatewayRoutes []orchestratorsnapshot.GatewayRoute,
	menus []orchestratorsnapshot.Menu,
	frontendRoutes []orchestratorsnapshot.FrontendRoute,
	healthChecks []ServiceComponent,
	dependencyEdges []orchestratorsnapshot.Edge,
) ([]ServiceTopologyNode, []ServiceTopologyEdge) {
	nodes := make([]ServiceTopologyNode, 0)
	edges := make([]ServiceTopologyEdge, 0)
	knownNodes := map[string]bool{}
	addNode := func(node ServiceTopologyNode) {
		if node.ID == "" || knownNodes[node.ID] {
			return
		}
		knownNodes[node.ID] = true
		nodes = append(nodes, node)
	}
	for _, service := range services {
		addNode(ServiceTopologyNode{
			ID:        service.ServiceID,
			ServiceID: service.ServiceID,
			Label:     firstNonEmpty(service.Name, service.ServiceID),
			Type:      "service",
			Status:    service.Status,
			Source:    "orchestrator_snapshot",
			Config:    json.RawMessage(`{}`),
		})
	}
	for _, edge := range dependencyEdges {
		edges = append(edges, ServiceTopologyEdge{
			ID:        edge.FromServiceID + "->" + edge.ToServiceID + ":" + edge.EdgeType,
			ServiceID: edge.FromServiceID,
			From:      edge.FromServiceID,
			To:        edge.ToServiceID,
			Type:      edge.EdgeType,
			Required:  edge.Required,
			Source:    "orchestrator_snapshot",
		})
	}
	for _, component := range components {
		id := topologyID(component.ServiceID, "component", component.ComponentID)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: component.ServiceID,
			Label:     component.ComponentID,
			Type:      component.Type,
			Status:    component.Status,
			Source:    "orchestrator_snapshot",
			Config:    component.Config,
		})
		edges = append(edges, ServiceTopologyEdge{
			ID:        component.ServiceID + "->" + id,
			ServiceID: component.ServiceID,
			From:      component.ServiceID,
			To:        id,
			Type:      "provides",
			Required:  false,
			Source:    "orchestrator_snapshot",
		})
	}
	for _, service := range services {
		id := topologyID(service.ServiceID, "service", service.ServiceID)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: service.ServiceID,
			Label:     firstNonEmpty(service.Name, service.ServiceID),
			Type:      "service",
			Status:    service.State,
			Source:    "status_view",
			Config:    mustRaw(map[string]any{"service_id": service.ServiceID, "runtime": service.Runtime, "lifecycle": service.Lifecycle, "health": service.Health, "routes": service.Routes}),
		})
		edges = append(edges, ServiceTopologyEdge{
			ID:        service.ServiceID + "->" + id + ":service-status",
			ServiceID: service.ServiceID,
			From:      service.ServiceID,
			To:        id,
			Type:      "status_view",
			Required:  service.Required,
			Source:    "status_view",
		})
		if service.HealthCheckID != "" {
			edges = append(edges, ServiceTopologyEdge{
				ID:        id + "->" + topologyID(service.ServiceID, "health", service.HealthCheckID),
				ServiceID: service.ServiceID,
				From:      id,
				To:        topologyID(service.ServiceID, "health", service.HealthCheckID),
				Type:      "health",
				Required:  service.Required,
				Source:    "status_view",
			})
		}
	}
	for _, worker := range workers {
		id := topologyID(worker.ServiceID, "worker", worker.ServiceID)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: worker.ServiceID,
			Label:     firstNonEmpty(worker.Name, worker.ServiceID),
			Type:      "worker",
			Status:    worker.State,
			Source:    "status_view",
			Config:    mustRaw(map[string]any{"service_id": worker.ServiceID, "runtime": worker.Runtime, "lifecycle": worker.Lifecycle, "health": worker.Health}),
		})
		edges = append(edges, ServiceTopologyEdge{
			ID:        worker.ServiceID + "->" + id + ":worker-status",
			ServiceID: worker.ServiceID,
			From:      worker.ServiceID,
			To:        id,
			Type:      "worker_status",
			Required:  worker.Required,
			Source:    "status_view",
		})
		if worker.HealthCheckID != "" {
			edges = append(edges, ServiceTopologyEdge{
				ID:        id + "->" + topologyID(worker.ServiceID, "health", worker.HealthCheckID),
				ServiceID: worker.ServiceID,
				From:      id,
				To:        topologyID(worker.ServiceID, "health", worker.HealthCheckID),
				Type:      "health",
				Required:  worker.Required,
				Source:    "status_view",
			})
		}
	}
	for _, route := range gatewayRoutes {
		id := topologyID(route.ServiceID, "gateway_route", route.Prefix)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: route.ServiceID,
			Label:     route.Prefix,
			Type:      "gateway_route",
			Status:    boolStatus(route.Enabled),
			Source:    "orchestrator_snapshot",
			Config:    mustRaw(map[string]any{"auth_mode": route.AuthMode, "target_service": route.TargetService}),
		})
		edges = append(edges, ServiceTopologyEdge{
			ID:        route.ServiceID + "->" + id,
			ServiceID: route.ServiceID,
			From:      route.ServiceID,
			To:        id,
			Type:      "routes",
			Required:  false,
			Source:    "orchestrator_snapshot",
		})
	}
	for _, menu := range menus {
		id := topologyID(menu.ServiceID, "menu", menu.MenuKey)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: menu.ServiceID,
			Label:     menu.Title,
			Type:      "menu",
			Status:    boolStatus(menu.Enabled),
			Source:    "orchestrator_snapshot",
			Config:    mustRaw(map[string]any{"route_path": menu.RoutePath, "required_permission": menu.RequiredPermission}),
		})
		edges = append(edges, ServiceTopologyEdge{
			ID:        menu.ServiceID + "->" + id,
			ServiceID: menu.ServiceID,
			From:      menu.ServiceID,
			To:        id,
			Type:      "menu",
			Required:  false,
			Source:    "orchestrator_snapshot",
		})
	}
	for _, route := range frontendRoutes {
		id := topologyID(route.ServiceID, "frontend_route", route.RoutePath)
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: route.ServiceID,
			Label:     firstNonEmpty(route.RouteName, route.RoutePath),
			Type:      "frontend_route",
			Status:    boolStatus(route.Enabled),
			Source:    "orchestrator_snapshot",
			Config:    mustRaw(map[string]any{"route_path": route.RoutePath, "component_key": route.ComponentKey, "required_permission": route.RequiredPermission}),
		})
	}
	for _, health := range healthChecks {
		id := topologyID(health.ServiceID, "health", health.ComponentID)
		if knownNodes[id] {
			continue
		}
		addNode(ServiceTopologyNode{
			ID:        id,
			ServiceID: health.ServiceID,
			Label:     health.ComponentID,
			Type:      "health_check",
			Status:    health.Status,
			Source:    "orchestrator_snapshot",
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
			addNode(ServiceTopologyNode{
				ID:        id,
				ServiceID: service.ServiceID,
				Label:     firstNonEmpty(item.Label, item.ID),
				Type:      item.Type,
				Status:    service.Status,
				Source:    "manifest",
				Config:    json.RawMessage(`{}`),
			})
			if serviceByID[service.ServiceID].ServiceID != "" {
				edges = append(edges, ServiceTopologyEdge{
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
			edges = append(edges, ServiceTopologyEdge{
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

func topologyWarnings(topology ServiceTopology) []string {
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

func manifestItem(serviceID string, id string, typ string, status string, enabled bool, config map[string]any) ServiceManifestItem {
	return ServiceManifestItem{
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
		return orchestratorsnapshot.StatusEnabled
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

func normalizeRequiredPermission(permission string) string {
	permission = strings.TrimSpace(permission)
	if strings.EqualFold(permission, "public") {
		return ""
	}
	return permission
}

func isSupportedRouteAuthMode(mode string) bool {
	switch mode {
	case "public", "user", "admin", "worker", "internal", "service", "workload":
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
		"/health",
		"/healthz",
		"/readyz",
		"/metrics",
		"/__ojos/extensions",
		"/internal/apis",
		"/api/auth",
		"/api/admin",
		"/api/admin/services",
		"/api/admin/health",
		"/api/health",
		"/api/internal",
		"/api/judge/worker",
		"/api/v1/contributions",
		"/api/v1/topologies",
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
	sort.Slice(snapshot.ServiceDefinitions, func(i, j int) bool {
		return snapshot.ServiceDefinitions[i].ServiceID < snapshot.ServiceDefinitions[j].ServiceID
	})
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
	sortServiceStatuses(snapshot.Services)
	sortServiceStatuses(snapshot.Workers)
	sort.Slice(snapshot.HealthChecks, func(i, j int) bool {
		return snapshot.HealthChecks[i].ComponentID < snapshot.HealthChecks[j].ComponentID
	})
	sort.Slice(snapshot.StorageBuckets, func(i, j int) bool { return manifestLess(snapshot.StorageBuckets[i], snapshot.StorageBuckets[j]) })
	sort.Slice(snapshot.Operations, func(i, j int) bool { return manifestLess(snapshot.Operations[i], snapshot.Operations[j]) })
	sort.Slice(snapshot.Topology.Nodes, func(i, j int) bool { return snapshot.Topology.Nodes[i].ID < snapshot.Topology.Nodes[j].ID })
	sort.Slice(snapshot.Topology.Edges, func(i, j int) bool { return snapshot.Topology.Edges[i].ID < snapshot.Topology.Edges[j].ID })
	sort.Slice(snapshot.Topology.ServiceDefinitions, func(i, j int) bool {
		return snapshot.Topology.ServiceDefinitions[i].ServiceID < snapshot.Topology.ServiceDefinitions[j].ServiceID
	})
	sort.Slice(snapshot.Topology.DependencyEdges, func(i, j int) bool {
		if snapshot.Topology.DependencyEdges[i].FromServiceID == snapshot.Topology.DependencyEdges[j].FromServiceID {
			return snapshot.Topology.DependencyEdges[i].ToServiceID < snapshot.Topology.DependencyEdges[j].ToServiceID
		}
		return snapshot.Topology.DependencyEdges[i].FromServiceID < snapshot.Topology.DependencyEdges[j].FromServiceID
	})
	sort.Strings(snapshot.Warnings)
}

func manifestLess(left ServiceManifestItem, right ServiceManifestItem) bool {
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
