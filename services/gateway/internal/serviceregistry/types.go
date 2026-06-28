package serviceregistry

import "encoding/json"

const (
	StatusEnabled = "ENABLED"

	KindKernel   = "kernel"
	KindPlatform = "platform"
	KindFeature  = "feature"
)

type Set struct {
	SetID       string `json:"set_id"`
	Name        string `json:"name"`
	Description string `json:"description"`
	SortOrder   int    `json:"sort_order"`
}

type Service struct {
	ServiceID   string          `json:"service_id"`
	SetID       string          `json:"set_id"`
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

type Installation struct {
	ServiceID  string          `json:"service_id"`
	Name       string          `json:"name"`
	Version    string          `json:"version"`
	Status     string          `json:"status"`
	Manifest   json.RawMessage `json:"manifest"`
	EnabledAt  string          `json:"enabled_at,omitempty"`
	DisabledAt string          `json:"disabled_at,omitempty"`
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
	ServiceID     string `json:"service_id"`
	Prefix        string `json:"prefix"`
	TargetService string `json:"target_service"`
	AuthMode      string `json:"auth_mode"`
	Enabled       bool   `json:"enabled"`
}

type Migration struct {
	ServiceID     string `json:"service_id"`
	Version       string `json:"version"`
	MigrationName string `json:"migration_name"`
	Checksum      string `json:"checksum"`
}

type Topology struct {
	Sets       []Set       `json:"sets"`
	Nodes      []Service   `json:"nodes"`
	Edges      []Edge      `json:"edges"`
	Components []Component `json:"components"`
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
	Installations  []Installation  `json:"installations"`
	HealthChecks   []Component     `json:"health_checks"`
}

type BootstrapData struct {
	Sets           []Set
	Services       []Service
	Edges          []Edge
	Components     []Component
	Installations  []Installation
	Permissions    []Permission
	Menus          []Menu
	FrontendRoutes []FrontendRoute
	GatewayRoutes  []GatewayRoute
	Migrations     []Migration
}
