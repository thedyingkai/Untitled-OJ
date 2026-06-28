package serviceruntime

import (
	"context"
	"errors"
	"net/http"
	"sort"
	"strings"
	"time"
)

const (
	ServiceStateDeclared  = "DECLARED"
	ServiceStateInstalled = "INSTALLED"
	ServiceStateEnabled   = "ENABLED"
	ServiceStateStarting  = "STARTING"
	ServiceStateRunning   = "RUNNING"
	ServiceStateDegraded  = "DEGRADED"
	ServiceStateStopping  = "STOPPING"
	ServiceStateStopped   = "STOPPED"
	ServiceStateFailed    = "FAILED"
	ServiceStateDisabled  = "DISABLED"
	ServiceStateUnknown   = "UNKNOWN"

	LifecycleManaged  = "managed"
	LifecycleMetadata = "metadata"
)

type RuntimeDriver interface {
	ListServices(context.Context, Snapshot) ([]RuntimeService, error)
	GetServiceState(context.Context, Snapshot, string) (RuntimeService, error)
	PlanStart(context.Context, Snapshot, string) (RuntimePlan, error)
	PlanStop(context.Context, Snapshot, string) (RuntimePlan, error)
	PlanRestart(context.Context, Snapshot, string) (RuntimePlan, error)
	PlanReload(context.Context, Snapshot, string) (RuntimePlan, error)
	PlanHealth(context.Context, Snapshot, string) (RuntimePlan, error)
	ApplyPlan(context.Context, RuntimePlan) (RuntimePlanResult, error)
}

type RuntimeService struct {
	OwnerServiceID string   `json:"owner_service_id"`
	ServiceID      string   `json:"service_id"`
	Name           string   `json:"name"`
	Kind           string   `json:"kind"`
	Lifecycle      string   `json:"lifecycle"`
	Runtime        string   `json:"runtime"`
	ComposeService string   `json:"compose_service,omitempty"`
	State          string   `json:"state"`
	Health         string   `json:"health"`
	Required       bool     `json:"required"`
	Routes         []string `json:"routes"`
	HealthCheckID  string   `json:"health_check_id,omitempty"`
	Status         string   `json:"status"`
	BlockedBy      []string `json:"blocked_by"`
	Warnings       []string `json:"warnings"`
}

type RuntimePlan struct {
	PlanID               string               `json:"plan_id"`
	OperationID          string               `json:"operation_id"`
	Action               string               `json:"action"`
	ServiceID            string               `json:"service_id"`
	Driver               string               `json:"driver"`
	CanApply             bool                 `json:"can_apply"`
	ApplyEnabled         bool                 `json:"apply_enabled"`
	RequiresConfirmation bool                 `json:"requires_confirmation"`
	DryRun               bool                 `json:"dry_run"`
	AllowedTargets       []string             `json:"allowed_targets"`
	Commands             []RuntimePlanCommand `json:"commands"`
	Affected             []string             `json:"affected"`
	BlockedBy            []string             `json:"blocked_by"`
	Warnings             []string             `json:"warnings"`
	CreatedAt            string               `json:"created_at"`
	ExpiresAt            string               `json:"expires_at"`
}

type RuntimePlanCommand struct {
	Kind string   `json:"kind"`
	Argv []string `json:"argv"`
}

type RuntimePlanResult struct {
	PlanID  string `json:"plan_id"`
	Status  string `json:"status"`
	Message string `json:"message"`
}

type ComposeDriver struct {
	TrustedServices        map[string]TrustedService
	AllowedComposeServices map[string]bool
	HTTPClient             *http.Client
	ApplyEnabled           bool
	ComposeFile            string
	EnvFile                string
	PlanTTL                time.Duration
}

func NewComposeDriver(trusted map[string]TrustedService, allowedComposeServices ...string) *ComposeDriver {
	normalized := normalizeTrustedServices(trusted)
	allowed := map[string]bool{}
	for serviceID := range normalized {
		allowed[serviceID] = true
	}
	for _, serviceID := range allowedComposeServices {
		serviceID = strings.TrimSpace(serviceID)
		if serviceID != "" {
			allowed[serviceID] = true
		}
	}
	return &ComposeDriver{
		TrustedServices:        normalized,
		AllowedComposeServices: allowed,
		HTTPClient:             &http.Client{Timeout: 2 * time.Second},
		ComposeFile:            "deploy/compose/docker-compose.yml",
		EnvFile:                ".env",
		PlanTTL:                5 * time.Minute,
	}
}

func (d *ComposeDriver) ListServices(ctx context.Context, snapshot Snapshot) ([]RuntimeService, error) {
	services := collectRuntimeServices(snapshot)
	for i := range services {
		services[i] = d.enrichServiceState(ctx, services[i])
	}
	sortRuntimeServices(services)
	return services, nil
}

func (d *ComposeDriver) GetServiceState(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimeService, error) {
	services, err := d.ListServices(ctx, snapshot)
	if err != nil {
		return RuntimeService{}, err
	}
	serviceID = strings.TrimSpace(serviceID)
	for _, service := range services {
		if service.ServiceID == serviceID {
			return service, nil
		}
	}
	return RuntimeService{}, errors.New("not found: runtime service")
}

func (d *ComposeDriver) PlanStart(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimePlan, error) {
	return d.plan(ctx, snapshot, serviceID, "start")
}

func (d *ComposeDriver) PlanStop(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimePlan, error) {
	return d.plan(ctx, snapshot, serviceID, "stop")
}

func (d *ComposeDriver) PlanRestart(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimePlan, error) {
	return d.plan(ctx, snapshot, serviceID, "restart")
}

func (d *ComposeDriver) PlanReload(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimePlan, error) {
	return d.plan(ctx, snapshot, serviceID, "reload")
}

func (d *ComposeDriver) PlanHealth(ctx context.Context, snapshot Snapshot, serviceID string) (RuntimePlan, error) {
	return d.plan(ctx, snapshot, serviceID, "health")
}

func (d *ComposeDriver) ApplyPlan(context.Context, RuntimePlan) (RuntimePlanResult, error) {
	return RuntimePlanResult{}, errors.New("not implemented: runtime apply-plan is disabled in L2 foundation")
}

func (d *ComposeDriver) plan(ctx context.Context, snapshot Snapshot, serviceID string, action string) (RuntimePlan, error) {
	service, err := d.GetServiceState(ctx, snapshot, serviceID)
	now := time.Now().UTC()
	ttl := d.PlanTTL
	if ttl <= 0 {
		ttl = 5 * time.Minute
	}
	composeFile := strings.TrimSpace(d.ComposeFile)
	if composeFile == "" {
		composeFile = "deploy/compose/docker-compose.yml"
	}
	envFile := strings.TrimSpace(d.EnvFile)
	if envFile == "" {
		envFile = ".env"
	}
	plan := RuntimePlan{
		PlanID:               "runtime-" + action + "-" + strings.TrimSpace(serviceID),
		OperationID:          "runtime-" + action + "-" + strings.TrimSpace(serviceID) + "-" + now.Format("20060102T150405.000000000Z"),
		Action:               action,
		ServiceID:            strings.TrimSpace(serviceID),
		Driver:               "compose",
		CanApply:             false,
		ApplyEnabled:         false,
		RequiresConfirmation: true,
		DryRun:               false,
		CreatedAt:            now.Format(time.RFC3339Nano),
		ExpiresAt:            now.Add(ttl).Format(time.RFC3339Nano),
	}
	if err != nil {
		plan.BlockedBy = append(plan.BlockedBy, err.Error())
		return plan, nil
	}
	plan.ServiceID = service.ServiceID
	plan.Affected = []string{service.ServiceID}
	plan.BlockedBy = append(plan.BlockedBy, service.BlockedBy...)
	if service.Lifecycle == LifecycleMetadata {
		plan.BlockedBy = append(plan.BlockedBy, "metadata lifecycle cannot "+action)
	}
	if service.Runtime != "compose" {
		plan.BlockedBy = append(plan.BlockedBy, "unsupported runtime "+service.Runtime)
	}
	if service.ComposeService == "" {
		plan.BlockedBy = append(plan.BlockedBy, "compose_service is required")
	}
	if !d.isAllowedService(service) {
		plan.BlockedBy = append(plan.BlockedBy, "service is not in trusted compose allowlist")
	}
	plan.AllowedTargets = []string{service.ComposeService}
	commandAction := action
	if action == "health" {
		commandAction = "ps"
	} else if action == "reload" {
		commandAction = "restart"
		plan.Warnings = append(plan.Warnings, "compose reload uses restart fallback")
	}
	if service.ComposeService != "" {
		argv := []string{"docker", "compose", "--env-file", envFile, "-f", composeFile, commandAction, service.ComposeService}
		plan.Commands = []RuntimePlanCommand{{Kind: "compose", Argv: argv}}
	}
	plan.Warnings = append(plan.Warnings, "Gateway generates operator plan only; Gateway/Web apply is disabled in L2 controlled apply")
	plan.CanApply = len(plan.BlockedBy) == 0
	return plan, nil
}

func (d *ComposeDriver) enrichServiceState(ctx context.Context, service RuntimeService) RuntimeService {
	if service.Lifecycle == "" {
		service.Lifecycle = LifecycleMetadata
	}
	if service.Kind == "" {
		service.Kind = "service"
	}
	if service.Runtime == "" {
		service.Runtime = "metadata"
	}
	if service.Lifecycle == LifecycleMetadata {
		service.State = ServiceStateDeclared
		service.Health = "metadata"
		service.Status = "metadata"
		return service
	}
	if service.Runtime != "compose" {
		service.State = ServiceStateUnknown
		service.Health = "unknown"
		service.Status = "unknown"
		service.BlockedBy = append(service.BlockedBy, "unsupported runtime "+service.Runtime)
		return service
	}
	trusted, hasTrustedUpstream := d.TrustedServices[service.ServiceID]
	if !d.isAllowedService(service) {
		service.State = ServiceStateUnknown
		service.Health = "unknown"
		service.Status = "blocked"
		service.BlockedBy = append(service.BlockedBy, "service is not in trusted compose allowlist")
		return service
	}
	if service.ComposeService == "" && hasTrustedUpstream {
		service.ComposeService = trusted.ServiceID
	}
	if !hasTrustedUpstream || strings.TrimSpace(trusted.UpstreamBase) == "" {
		service.State = ServiceStateUnknown
		service.Health = "unknown"
		service.Status = "unknown"
		service.Warnings = append(service.Warnings, "no HTTP health endpoint is configured for this compose service")
		return service
	}
	health, state := d.checkHTTPHealth(ctx, trusted.UpstreamBase)
	service.Health = health
	service.State = state
	service.Status = strings.ToLower(state)
	return service
}

func (d *ComposeDriver) isAllowedService(service RuntimeService) bool {
	if d == nil {
		return false
	}
	if d.AllowedComposeServices[service.ServiceID] {
		return true
	}
	if service.ComposeService != "" && d.AllowedComposeServices[service.ComposeService] {
		return true
	}
	return false
}

func collectRuntimeServices(snapshot Snapshot) []RuntimeService {
	out := make([]RuntimeService, 0, len(snapshot.Services)+len(snapshot.Workers))
	out = append(out, snapshot.Services...)
	out = append(out, snapshot.Workers...)
	return out
}

func (d *ComposeDriver) checkHTTPHealth(ctx context.Context, upstream string) (string, string) {
	client := d.HTTPClient
	if client == nil {
		client = &http.Client{Timeout: 2 * time.Second}
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(upstream, "/")+"/health", nil)
	if err != nil {
		return "unknown", ServiceStateUnknown
	}
	resp, err := client.Do(req)
	if err != nil {
		return "error", ServiceStateStopped
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 500 {
		return "error", ServiceStateFailed
	}
	if resp.StatusCode >= 400 {
		return "degraded", ServiceStateDegraded
	}
	return "ok", ServiceStateRunning
}

func RuntimeServiceStates(services []RuntimeService) map[string]RuntimeService {
	out := make(map[string]RuntimeService, len(services))
	for _, service := range services {
		out[service.ServiceID] = service
	}
	return out
}

func sortRuntimeServices(services []RuntimeService) {
	sort.Slice(services, func(i, j int) bool {
		if services[i].ServiceID == services[j].ServiceID {
			return services[i].ServiceID < services[j].ServiceID
		}
		return services[i].ServiceID < services[j].ServiceID
	})
}
