package moduleregistry

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

type Module struct {
	ModuleID    string          `json:"module_id"`
	SetID       string          `json:"set_id"`
	Name        string          `json:"name"`
	Version     string          `json:"version"`
	Status      string          `json:"status"`
	Kind        string          `json:"kind"`
	Description string          `json:"description"`
	Manifest    json.RawMessage `json:"manifest,omitempty"`
}

type Edge struct {
	FromModuleID      string `json:"from_module_id"`
	ToModuleID        string `json:"to_module_id"`
	EdgeType          string `json:"edge_type"`
	VersionConstraint string `json:"version_constraint"`
	Required          bool   `json:"required"`
}

type Component struct {
	ModuleID      string          `json:"module_id"`
	ComponentID   string          `json:"component_id"`
	ComponentType string          `json:"component_type"`
	Status        string          `json:"status"`
	Config        json.RawMessage `json:"config"`
}

type Installation struct {
	ModuleID   string          `json:"module_id"`
	Name       string          `json:"name"`
	Version    string          `json:"version"`
	Status     string          `json:"status"`
	Manifest   json.RawMessage `json:"manifest"`
	EnabledAt  string          `json:"enabled_at,omitempty"`
	DisabledAt string          `json:"disabled_at,omitempty"`
}

type Permission struct {
	ModuleID      string `json:"module_id"`
	PermissionKey string `json:"permission_key"`
	Description   string `json:"description"`
}

type Menu struct {
	ModuleID           string `json:"module_id"`
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
	ModuleID           string `json:"module_id"`
	RoutePath          string `json:"route_path"`
	RouteName          string `json:"route_name"`
	ComponentKey       string `json:"component_key"`
	RequiredPermission string `json:"required_permission"`
	Enabled            bool   `json:"enabled"`
}

type GatewayRoute struct {
	ModuleID      string `json:"module_id"`
	Prefix        string `json:"prefix"`
	TargetService string `json:"target_service"`
	AuthMode      string `json:"auth_mode"`
	Enabled       bool   `json:"enabled"`
}

type Migration struct {
	ModuleID      string `json:"module_id"`
	Version       string `json:"version"`
	MigrationName string `json:"migration_name"`
	Checksum      string `json:"checksum"`
}

type Topology struct {
	Sets       []Set       `json:"sets"`
	Nodes      []Module    `json:"nodes"`
	Edges      []Edge      `json:"edges"`
	Components []Component `json:"components"`
}

type Detail struct {
	Module         Module          `json:"module"`
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
	Modules        []Module
	Edges          []Edge
	Components     []Component
	Installations  []Installation
	Permissions    []Permission
	Menus          []Menu
	FrontendRoutes []FrontendRoute
	GatewayRoutes  []GatewayRoute
	Migrations     []Migration
}
