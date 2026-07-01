package orchestratorsnapshot

import "encoding/json"

const (
	StatusEnabled = "ENABLED"

	KindGateway       = "gateway"
	KindFrontend      = "frontend"
	KindBackendAPI    = "backend-api"
	KindBackendWorker = "backend-worker"
	KindDatabase      = "database"
	KindCache         = "cache"
	KindStorage       = "storage"
	KindExternal      = "external"
	KindAgent         = "agent"
)

type EndpointGroup struct {
	ServiceName   string   `json:"service_name"`
	Selector      string   `json:"selector"`
	EndpointCount int      `json:"endpoint_count"`
	Endpoints     []string `json:"endpoints"`
}

type Service struct {
	ServiceID   string          `json:"service_id"`
	Name        string          `json:"name"`
	Version     string          `json:"version"`
	Status      string          `json:"status"`
	Kind        string          `json:"kind"`
	Description string          `json:"description"`
	Manifest    json.RawMessage `json:"manifest,omitempty"`
}

type Edge struct {
	FromServiceID     string `json:"from_service_id"`
	ToServiceID       string `json:"to_service_id"`
	EdgeType          string `json:"edge_type"`
	VersionConstraint string `json:"version_constraint"`
	Required          bool   `json:"required"`
}

type Component struct {
	ServiceID     string          `json:"service_id"`
	ComponentID   string          `json:"component_id"`
	ComponentType string          `json:"component_type"`
	Status        string          `json:"status"`
	Config        json.RawMessage `json:"config"`
}

func (c *Component) UnmarshalJSON(data []byte) error {
	type alias Component
	var raw struct {
		alias
		Type string `json:"type"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	*c = Component(raw.alias)
	if c.ComponentType == "" {
		c.ComponentType = raw.Type
	}
	return nil
}

type Endpoint struct {
	Endpoint    string          `json:"endpoint"`
	ServiceID   string          `json:"service_id"`
	Protocol    string          `json:"protocol"`
	HealthPath  string          `json:"health_path"`
	Health      string          `json:"health"`
	Reachable   bool            `json:"reachable"`
	DisplayName string          `json:"display_name"`
	Note        string          `json:"note"`
	Config      json.RawMessage `json:"config"`
}

type Permission struct {
	ServiceID     string `json:"service_id"`
	PermissionKey string `json:"permission_key"`
	Description   string `json:"description"`
}

type Menu struct {
	ServiceID          string `json:"service_id"`
	MenuKey            string `json:"menu_key"`
	Title              string `json:"title"`
	RoutePath          string `json:"route_path"`
	Icon               string `json:"icon"`
	ParentKey          string `json:"parent_key"`
	SortOrder          int    `json:"sort_order"`
	RequiredPermission string `json:"required_permission"`
	Enabled            bool   `json:"enabled"`
}

type FrontendRoute struct {
	ServiceID          string `json:"service_id"`
	RoutePath          string `json:"route_path"`
	RouteName          string `json:"route_name"`
	ComponentKey       string `json:"component_key"`
	RequiredPermission string `json:"required_permission"`
	Enabled            bool   `json:"enabled"`
}

type GatewayRoute struct {
	ApiID              string `json:"api_id,omitempty"`
	NodeID             string `json:"node_id,omitempty"`
	ProviderNodeID     string `json:"provider_node_id,omitempty"`
	ProviderHostIP     string `json:"provider_host_ip,omitempty"`
	ProviderService    string `json:"provider_service_name,omitempty"`
	ProviderEndpoint   string `json:"provider_endpoint,omitempty"`
	VisibilitySource   string `json:"visibility_source,omitempty"`
	Distance           int    `json:"distance,omitempty"`
	ServiceID          string `json:"service_id"`
	Prefix             string `json:"prefix"`
	TargetService      string `json:"target_service"`
	UpstreamBase       string `json:"upstream_base,omitempty"`
	AuthMode           string `json:"auth_mode"`
	RequiredPermission string `json:"required_permission,omitempty"`
	StripPrefix        string `json:"strip_prefix,omitempty"`
	RewritePrefix      string `json:"rewrite_prefix,omitempty"`
	HealthCheckID      string `json:"health_check_id,omitempty"`
	Enabled            bool   `json:"enabled"`
}

type Migration struct {
	ServiceID     string `json:"service_id"`
	Version       string `json:"version"`
	MigrationName string `json:"migration_name"`
	Checksum      string `json:"checksum"`
}

type Topology struct {
	EndpointGroups []EndpointGroup `json:"endpoint_groups"`
	Nodes          []Service       `json:"nodes"`
	Edges          []Edge          `json:"edges"`
	Components     []Component     `json:"components"`
}

type OrchestratorSnapshotData struct {
	ServiceDefinitions []Service       `json:"service_definitions"`
	Endpoints          []Endpoint      `json:"endpoints"`
	Permissions        []Permission    `json:"permissions"`
	Menus              []Menu          `json:"menus"`
	FrontendRoutes     []FrontendRoute `json:"frontend_routes"`
	GatewayRoutes      []GatewayRoute  `json:"gateway_routes"`
	Components         []Component     `json:"components"`
	HealthChecks       []Component     `json:"health_checks"`
	Topology           ServiceTopology `json:"topology"`
}

type ServiceTopology struct {
	DependencyEdges []Edge `json:"dependency_edges"`
}

type Detail struct {
	Service        Service         `json:"service"`
	Dependencies   []Edge          `json:"dependencies"`
	Dependents     []Edge          `json:"dependents"`
	Components     []Component     `json:"components"`
	Permissions    []Permission    `json:"permissions"`
	Menus          []Menu          `json:"menus"`
	FrontendRoutes []FrontendRoute `json:"frontend_routes"`
	GatewayRoutes  []GatewayRoute  `json:"gateway_routes"`
	Endpoints      []Endpoint      `json:"endpoints"`
	HealthChecks   []Component     `json:"health_checks"`
}

type SnapshotData struct {
	Services       []Service
	Edges          []Edge
	Components     []Component
	Endpoints      []Endpoint
	Permissions    []Permission
	Menus          []Menu
	FrontendRoutes []FrontendRoute
	GatewayRoutes  []GatewayRoute
	Migrations     []Migration
}
