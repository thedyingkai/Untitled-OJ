package servicestatus

import (
	"context"
	"errors"
	"net/http"
	"sort"
	"strings"
	"time"
)

const (
	ServiceStatusDeclared  = "DECLARED"
	ServiceStatusInstalled = "INSTALLED"
	ServiceStatusEnabled   = "ENABLED"
	ServiceStatusStarting  = "STARTING"
	ServiceStatusRunning   = "RUNNING"
	ServiceStatusDegraded  = "DEGRADED"
	ServiceStatusStopping  = "STOPPING"
	ServiceStatusStopped   = "STOPPED"
	ServiceStatusFailed    = "FAILED"
	ServiceStatusDisabled  = "DISABLED"
	ServiceStatusUnknown   = "UNKNOWN"

	LifecycleManaged  = "managed"
	LifecycleMetadata = "metadata"
)

type ServiceStatusDriver interface {
	ListServices(context.Context, Snapshot) ([]ServiceStatus, error)
	GetServiceStatus(context.Context, Snapshot, string) (ServiceStatus, error)
}

type ServiceStatus struct {
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

type ComposeDriver struct {
	TrustedServices        map[string]TrustedService
	AllowedComposeServices map[string]bool
	HTTPClient             *http.Client
	ComposeFile            string
	EnvFile                string
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
	}
}

func (d *ComposeDriver) ListServices(ctx context.Context, snapshot Snapshot) ([]ServiceStatus, error) {
	services := collectServiceStatuses(snapshot)
	for i := range services {
		services[i] = d.enrichServiceStatus(ctx, services[i])
	}
	sortServiceStatuses(services)
	return services, nil
}

func (d *ComposeDriver) GetServiceStatus(ctx context.Context, snapshot Snapshot, serviceID string) (ServiceStatus, error) {
	services, err := d.ListServices(ctx, snapshot)
	if err != nil {
		return ServiceStatus{}, err
	}
	serviceID = strings.TrimSpace(serviceID)
	for _, service := range services {
		if service.ServiceID == serviceID {
			return service, nil
		}
	}
	return ServiceStatus{}, errors.New("not found: Service Status")
}

func (d *ComposeDriver) enrichServiceStatus(ctx context.Context, service ServiceStatus) ServiceStatus {
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
		service.State = ServiceStatusDeclared
		service.Health = "metadata"
		service.Status = "metadata"
		return service
	}
	if service.Runtime != "compose" {
		service.State = ServiceStatusUnknown
		service.Health = "unknown"
		service.Status = "unknown"
		service.BlockedBy = append(service.BlockedBy, "unsupported runtime "+service.Runtime)
		return service
	}
	trusted, hasTrustedUpstream := d.TrustedServices[service.ServiceID]
	if !d.isAllowedService(service) {
		service.State = ServiceStatusUnknown
		service.Health = "unknown"
		service.Status = "blocked"
		service.BlockedBy = append(service.BlockedBy, "service is not in trusted compose allowlist")
		return service
	}
	if service.ComposeService == "" && hasTrustedUpstream {
		service.ComposeService = trusted.ServiceID
	}
	if !hasTrustedUpstream || strings.TrimSpace(trusted.UpstreamBase) == "" {
		service.State = ServiceStatusUnknown
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

func (d *ComposeDriver) isAllowedService(service ServiceStatus) bool {
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

func collectServiceStatuses(snapshot Snapshot) []ServiceStatus {
	out := make([]ServiceStatus, 0, len(snapshot.Services)+len(snapshot.Workers))
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
		return "unknown", ServiceStatusUnknown
	}
	resp, err := client.Do(req)
	if err != nil {
		return "error", ServiceStatusStopped
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 500 {
		return "error", ServiceStatusFailed
	}
	if resp.StatusCode >= 400 {
		return "degraded", ServiceStatusDegraded
	}
	return "ok", ServiceStatusRunning
}

func ServiceStatusesByID(services []ServiceStatus) map[string]ServiceStatus {
	out := make(map[string]ServiceStatus, len(services))
	for _, service := range services {
		out[service.ServiceID] = service
	}
	return out
}

func sortServiceStatuses(services []ServiceStatus) {
	sort.Slice(services, func(i, j int) bool {
		if services[i].ServiceID == services[j].ServiceID {
			return services[i].ServiceID < services[j].ServiceID
		}
		return services[i].ServiceID < services[j].ServiceID
	})
}
