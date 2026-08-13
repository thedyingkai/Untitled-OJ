package orchestratorsnapshot

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

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
	ApiID                string `json:"api_id,omitempty"`
	BindingID            string `json:"binding_id,omitempty"`
	ConsumerDeploymentID string `json:"consumer_deployment_id,omitempty"`
	CredentialGeneration uint64 `json:"credential_generation,omitempty"`
	TimeoutMS            uint64 `json:"timeout_ms,omitempty"`
	NodeID               string `json:"node_id,omitempty"`
	ProviderNodeID       string `json:"provider_node_id,omitempty"`
	ProviderHostIP       string `json:"provider_host_ip,omitempty"`
	ProviderService      string `json:"provider_service_name,omitempty"`
	ProviderEndpoint     string `json:"provider_endpoint,omitempty"`
	VisibilitySource     string `json:"visibility_source,omitempty"`
	Distance             int    `json:"distance,omitempty"`
	ServiceID            string `json:"service_id"`
	Prefix               string `json:"prefix"`
	TargetService        string `json:"target_service"`
	UpstreamBase         string `json:"upstream_base,omitempty"`
	AuthMode             string `json:"auth_mode"`
	RequiredPermission   string `json:"required_permission,omitempty"`
	StripPrefix          string `json:"strip_prefix,omitempty"`
	RewritePrefix        string `json:"rewrite_prefix,omitempty"`
	HealthCheckID        string `json:"health_check_id,omitempty"`
	Enabled              bool   `json:"enabled"`
}

// ContributionSnapshot is the deployment-scoped, atomically published route
// projection consumed by Gateway. Only routes from active heads are present;
// Enabled additionally reflects the runtime evidence gate.
type ContributionSnapshot struct {
	SchemaVersion         string                             `json:"schema_version"`
	Digest                string                             `json:"digest"`
	ScopeID               string                             `json:"scope_id"`
	Acknowledgements      []ContributionAcknowledgement      `json:"acknowledgements"`
	Revisions             []ContributionRevision             `json:"revisions"`
	GatewayRoutes         []ContributionGatewayRoute         `json:"gateway_routes"`
	PermissionDefinitions []ContributionPermissionDefinition `json:"permission_definitions"`
	UserFrontendModules   []ContributionFrontendModule       `json:"user_frontend_modules"`
	AdminFrontendModules  []ContributionFrontendModule       `json:"admin_frontend_modules"`
}

// ContributionAcknowledgement is an exact observation obligation emitted by
// Orchestrator as part of the signed snapshot digest. Consumers must return the
// array unchanged after the complete snapshot has been applied locally.
type ContributionAcknowledgement struct {
	ActivationID        string  `json:"activation_id"`
	ServiceID           string  `json:"service_id"`
	CandidateRevisionID string  `json:"candidate_revision_id"`
	CandidateGeneration uint64  `json:"candidate_generation"`
	ExpectedState       string  `json:"expected_state"`
	ObservedRevisionID  *string `json:"observed_revision_id"`
	ObservedGeneration  *uint64 `json:"observed_generation"`
}

type ContributionPermissionDefinition struct {
	ServiceID   string `json:"service_id"`
	RevisionID  string `json:"revision_id"`
	Generation  uint64 `json:"generation"`
	Key         string `json:"key"`
	Title       string `json:"title"`
	Description string `json:"description"`
}

type ContributionFrontendModule struct {
	ServiceID         string `json:"service_id"`
	DeploymentID      string `json:"deployment_id"`
	RevisionID        string `json:"revision_id"`
	Generation        uint64 `json:"generation"`
	Target            string `json:"target"`
	ModuleID          string `json:"module_id"`
	SurfaceID         string `json:"surface_id"`
	Route             string `json:"route"`
	MenuLabel         string `json:"menu_label"`
	Menu              bool   `json:"menu"`
	Order             int    `json:"order"`
	Permission        string `json:"permission,omitempty"`
	Artifact          string `json:"artifact"`
	HostAPIRange      string `json:"host_api_range"`
	ManifestDigest    string `json:"manifest_digest"`
	ManifestReference string `json:"manifest_reference"`
	BundleDigest      string `json:"bundle_digest"`
	BundleReference   string `json:"bundle_reference"`
	Enabled           bool   `json:"enabled"`
}

type ContributionRevision struct {
	ServiceID    string `json:"service_id"`
	DeploymentID string `json:"deployment_id"`
	RevisionID   string `json:"revision_id"`
	Generation   uint64 `json:"generation"`
	RuntimeReady bool   `json:"runtime_ready"`
}

type ContributionGatewayRoute struct {
	ServiceID       string           `json:"service_id"`
	DeploymentID    string           `json:"deployment_id"`
	RevisionID      string           `json:"revision_id"`
	Generation      uint64           `json:"generation"`
	Audience        string           `json:"audience"`
	Method          string           `json:"method"`
	Path            string           `json:"path"`
	ApiID           string           `json:"api_id"`
	OperationID     string           `json:"operation_id"`
	ProviderPath    string           `json:"provider_path"`
	Auth            string           `json:"auth"`
	Permission      string           `json:"permission,omitempty"`
	PermissionScope *PermissionScope `json:"permission_scope,omitempty"`
	UpstreamBase    string           `json:"upstream_base,omitempty"`
	Enabled         bool             `json:"enabled"`
}

// PermissionScope is emitted by the signed contribution snapshot. System
// scopes use Kind=system; resource scopes derive ScopeID only from the named
// path parameter on the matched external route.
type PermissionScope struct {
	Kind          string
	Type          string
	PathParameter string
}

func (s *PermissionScope) UnmarshalJSON(data []byte) error {
	var scalar string
	if err := json.Unmarshal(data, &scalar); err == nil {
		if scalar != "system" {
			return fmt.Errorf("unsupported permission scope %q", scalar)
		}
		*s = PermissionScope{Kind: "system", Type: "system"}
		return nil
	}
	var object struct {
		Type          string `json:"type"`
		PathParameter string `json:"pathParameter"`
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&object); err != nil {
		return fmt.Errorf("decode permission scope: %w", err)
	}
	if !validPermissionScopeType(strings.TrimSpace(object.Type)) || strings.TrimSpace(object.Type) == "system" || strings.TrimSpace(object.PathParameter) == "" {
		return errors.New("resource permission scope requires type and pathParameter")
	}
	*s = PermissionScope{
		Kind:          "path_parameter",
		Type:          strings.TrimSpace(object.Type),
		PathParameter: strings.TrimSpace(object.PathParameter),
	}
	return nil
}

func (s PermissionScope) MarshalJSON() ([]byte, error) {
	if s.Kind == "system" && s.Type == "system" && s.PathParameter == "" {
		return json.Marshal("system")
	}
	if s.Kind != "path_parameter" || !validPermissionScopeType(s.Type) || s.Type == "system" || strings.TrimSpace(s.PathParameter) == "" {
		return nil, errors.New("invalid permission scope")
	}
	return json.Marshal(struct {
		Type          string `json:"type"`
		PathParameter string `json:"pathParameter"`
	}{Type: s.Type, PathParameter: s.PathParameter})
}

func validPermissionScopeType(value string) bool {
	if value == "" || len(value) > 128 || value[0] < 'a' || value[0] > 'z' {
		return false
	}
	separator := false
	for _, character := range value[1:] {
		switch {
		case character >= 'a' && character <= 'z', character >= '0' && character <= '9':
			separator = false
		case character == '.', character == '_', character == '-':
			if separator {
				return false
			}
			separator = true
		default:
			return false
		}
	}
	return !separator
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
