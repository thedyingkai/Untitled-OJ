// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"

	"ojos-gateway/internal/authclient"
	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	gtopology "ojos-gateway/internal/topologyprojection"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/security/workload"
	"ojos-shared/servicecontext"

	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"

	"github.com/redis/go-redis/v9"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	Redis  *redis.Client
	Tracer *sdktrace.TracerProvider

	Proxy               http.HandlerFunc
	ServiceProxy        *proxy.ServiceProxy
	ServiceStatusDriver servicestatus.ServiceStatusDriver
	RouteTableOptions   servicestatus.RouteTableOptions
	InternalSigner      *internalauth.Signer
	Orchestrator        *orchestratorsnapshot.Client
	AuthClient          *authclient.Client
	PermissionChecker   sharedperm.UserChecker
	Context             *servicecontext.ContextProvider
	TopologyProjection  *gtopology.Store

	contributionCancel  context.CancelFunc
	contributionDone    chan struct{}
	contributionMu      sync.Mutex
	contributionDigest  string
	contributionAcked   string
	contributionPending *orchestratorsnapshot.ContributionSnapshot
	contributionReady   bool
	contributionError   string
}

const contributionSnapshotPollInterval = 5 * time.Second
const permissionBindingName = sharedperm.DefaultPermissionCheckApiID

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		log.Fatalf("configure Gateway: %v", err)
	}
	if err := validateWorkloadIdentityConfig(
		c.WorkloadIdentity,
		productionModeEnabled(),
	); err != nil {
		log.Fatalf("configure workload identity: %v", err)
	}

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}

	redisOptions, err := redis.ParseURL(c.Redis.Url)
	if err != nil {
		log.Fatalf("parse redis url failed: %v", err)
	}
	redisClient := redis.NewClient(redisOptions)
	if err := redisClient.Ping(ctx).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
	}

	var internalSigner *internalauth.Signer
	if c.InternalAuth.Enabled {
		zlog.Warn("gateway internal request signing is disabled until internal auth keys are provided by a service-owned API")
	}

	authClient := authclient.New(c.AuthService.Endpoint)
	var contextProvider *servicecontext.ContextProvider
	var permissionChecker sharedperm.UserChecker
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		log.Fatalf("load managed Service Context failed: %v", err)
	}
	if contextValue != nil {
		if err := contextValue.RequireService("gateway"); err != nil {
			log.Fatalf("validate managed Service Context failed: %v", err)
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			if contextProvider != nil {
				_ = contextProvider.Close()
			}
			log.Fatalf("configure permission ApiBinding failed: %v", err)
		}
	} else if managedEnvironment() {
		log.Fatal("managed Gateway requires an Agent Service Context")
	}
	serviceProxy, err := proxy.NewServiceProxy(c.Proxy.Routes, c.Proxy.TrustedServices, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		log.Fatalf("init proxy failed: %v", err)
	}
	serviceProxy.SetNodeID(c.Orchestrator.NodeID)
	if strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) != "" {
		workloadVerifier, verifyErr := workloadIdentityVerifier(c.WorkloadIdentity)
		if verifyErr != nil {
			log.Fatalf("configure workload identity verifier failed: %v", verifyErr)
		}
		serviceProxy.SetWorkloadVerifier(workloadVerifier)
	}
	serviceProxy.SetPermissionChecker(func(ctx context.Context, authHeader string, caller proxy.PermissionCheckCaller, permissionCode string) (bool, error) {
		if permissionChecker != nil {
			return permissionChecker.HasUserPermission(ctx, caller.UserID, permissionCode, sharedperm.Scope{
				Type: caller.ScopeType,
				ID:   caller.ScopeID,
			})
		}
		return authClient.HasPermission(ctx, authHeader, authclient.PermissionCaller{
			Type:    caller.Type,
			UserID:  caller.UserID,
			Service: caller.Service,
			NodeID:  caller.NodeID,
			APIID:   caller.APIID,
		}, permissionCode, caller.ScopeType, caller.ScopeID)
	})
	routeTableOptions := routeTableOptionsFromConfig(c.Proxy)
	serviceStatusDriver := servicestatus.NewComposeDriver(routeTableOptions.TrustedServices, c.ServiceStatus.ComposeServices...)
	orchestratorClient := orchestratorsnapshot.NewClient(
		c.Orchestrator.Endpoint,
		c.Orchestrator.InternalToken,
		c.Orchestrator.ContributionAckToken,
	)
	topologyProjection := gtopology.NewStore(redisClient, serviceProxy)
	if err := topologyProjection.Recover(ctx); err != nil {
		log.Fatalf("recover Gateway topology projections failed: %v", err)
	}
	var snapshot servicestatus.Snapshot
	if strings.TrimSpace(c.Orchestrator.NodeID) != "" {
		var table servicestatus.RouteTable
		if err := orchestratorClient.DecodeNodeOrchestratorRoutes(ctx, c.Orchestrator.NodeID, true, &table); err == nil {
			serviceProxy.SetRouteTable(table)
		} else {
			zlog.Warn("orchestrator node effective route table is unavailable; gateway starts degraded", zap.Error(err))
		}
	} else if err := orchestratorClient.DecodeOrchestratorSnapshot(ctx, false, &snapshot); err == nil {
		if services, serviceErr := serviceStatusDriver.ListServices(ctx, snapshot); serviceErr == nil {
			snapshot.Services = filterServiceStatusesByKind(services, false)
			snapshot.Workers = filterServiceStatusesByKind(services, true)
			routeTableOptions.ServiceStatuses = servicestatus.ServiceStatusesByID(snapshot.Services)
		}
		serviceProxy.SetRouteTable(servicestatus.BuildRouteTableWithOptions(snapshot, routeTableOptions))
	} else {
		zlog.Warn("orchestrator service snapshot is unavailable; gateway starts degraded", zap.Error(err))
	}
	serviceContext := &ServiceContext{
		Config:              c,
		Logger:              zlog,
		Redis:               redisClient,
		Tracer:              tp,
		Proxy:               serviceProxy.ServeHTTP,
		ServiceProxy:        serviceProxy,
		ServiceStatusDriver: serviceStatusDriver,
		RouteTableOptions:   routeTableOptions,
		InternalSigner:      internalSigner,
		Orchestrator:        orchestratorClient,
		AuthClient:          authClient,
		PermissionChecker:   permissionChecker,
		Context:             contextProvider,
		TopologyProjection:  topologyProjection,
	}
	if orchestratorClient.Configured() {
		if err := serviceContext.reloadContributionSnapshot(ctx); err != nil {
			zlog.Warn("orchestrator contribution snapshot is unavailable; contribution routes start degraded", zap.Error(err))
		}
		serviceContext.startContributionSnapshotReconciler(contributionSnapshotPollInterval)
	}
	return serviceContext
}

func (s *ServiceContext) reloadContributionSnapshot(ctx context.Context) error {
	if s == nil || s.Orchestrator == nil || s.ServiceProxy == nil {
		return fmt.Errorf("contribution snapshot consumer is not configured")
	}
	snapshot, err := s.Orchestrator.ContributionSnapshot(ctx)
	if err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	unchanged := snapshot.Digest != "" && snapshot.Digest == s.contributionDigest
	pending := s.contributionPending
	s.contributionMu.Unlock()
	if unchanged {
		if pending == nil || pending.Digest != snapshot.Digest {
			return nil
		}
		return s.acknowledgeContributionSnapshot(ctx, *pending)
	}
	table, err := servicestatus.ContributionRouteTable(snapshot)
	if err != nil {
		s.recordContributionError(err)
		return err
	}
	if err := s.ServiceProxy.ApplyContributionSnapshot(table, snapshot); err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	s.contributionDigest = snapshot.Digest
	s.contributionPending = &snapshot
	s.contributionReady = true
	s.contributionError = ""
	s.contributionMu.Unlock()
	return s.acknowledgeContributionSnapshot(ctx, snapshot)
}

func (s *ServiceContext) acknowledgeContributionSnapshot(ctx context.Context, snapshot orchestratorsnapshot.ContributionSnapshot) error {
	if s.Orchestrator == nil || !s.Orchestrator.ContributionAcknowledgementsConfigured() {
		return nil
	}
	if err := s.Orchestrator.AcknowledgeContributionSnapshot(ctx, snapshot); err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	if s.contributionPending != nil && s.contributionPending.Digest == snapshot.Digest {
		s.contributionPending = nil
		s.contributionAcked = snapshot.Digest
		s.contributionError = ""
	}
	s.contributionMu.Unlock()
	return nil
}

func (s *ServiceContext) recordContributionError(err error) {
	if s == nil || err == nil {
		return
	}
	s.contributionMu.Lock()
	s.contributionError = err.Error()
	s.contributionMu.Unlock()
}

func (s *ServiceContext) startContributionSnapshotReconciler(interval time.Duration) {
	if s == nil || interval <= 0 {
		return
	}
	s.contributionMu.Lock()
	if s.contributionCancel != nil {
		s.contributionMu.Unlock()
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	s.contributionCancel = cancel
	s.contributionDone = done
	s.contributionMu.Unlock()
	go func() {
		defer close(done)
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if err := s.reloadContributionSnapshot(ctx); err != nil && s.Logger != nil {
					s.Logger.Warn("orchestrator contribution snapshot reconciliation failed; retaining active revision", zap.Error(err))
				}
			}
		}
	}()
}

func routeTableOptionsFromConfig(cfg config.ProxyConfig) servicestatus.RouteTableOptions {
	trusted := make(map[string]servicestatus.TrustedService)
	for _, item := range cfg.TrustedServices {
		if strings.TrimSpace(item.ServiceID) == "" {
			continue
		}
		trusted[item.ServiceID] = servicestatus.TrustedService{
			ServiceID:     item.ServiceID,
			UpstreamBase:  item.Target,
			StripPrefix:   item.StripPrefix,
			RewritePrefix: item.RewritePrefix,
			HealthCheckID: item.HealthCheckID,
		}
	}
	for _, route := range cfg.Routes {
		serviceID := inferServiceID(route.Target)
		if serviceID == "" {
			continue
		}
		if _, ok := trusted[serviceID]; ok {
			continue
		}
		trusted[serviceID] = servicestatus.TrustedService{
			ServiceID:     serviceID,
			UpstreamBase:  route.Target,
			StripPrefix:   route.StripPrefix,
			HealthCheckID: serviceID + "-health",
		}
	}
	return servicestatus.RouteTableOptions{
		TrustedServices: trusted,
	}
}

func filterServiceStatusesByKind(items []servicestatus.ServiceStatus, workers bool) []servicestatus.ServiceStatus {
	out := make([]servicestatus.ServiceStatus, 0, len(items))
	for _, item := range items {
		if (item.Kind == "worker") == workers {
			out = append(out, item)
		}
	}
	return out
}

func applyEnvOverrides(c *config.Config) error {
	if c == nil {
		return errors.New("Gateway config is nil")
	}
	managed := managedEnvironment()
	bootstrap := platformBootstrapEnvironment()
	production := productionModeEnabled()
	if managed && bootstrap {
		return errors.New("Gateway cannot be both Agent-managed and a platform bootstrap service")
	}
	if managed {
		return applyManagedEnv(c)
	}
	if bootstrap {
		return applyPlatformBootstrapEnv(c)
	}
	if production {
		return errors.New("production Gateway requires OJOS_MANAGED_WORKLOAD=1 or OJOS_PLATFORM_BOOTSTRAP=1")
	}
	if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
		c.Redis.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_ENDPOINT")); value != "" {
		c.Orchestrator.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_INTERNAL_TOKEN")); value != "" {
		c.Orchestrator.InternalToken = value
	}
	if value := strings.TrimSpace(os.Getenv("CONTRIBUTION_ACK_TOKEN")); value != "" {
		c.Orchestrator.ContributionAckToken = value
	}
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_NODE_ID")); value != "" {
		c.Orchestrator.NodeID = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE")); value != "" {
		c.WorkloadIdentity.PublicKeyFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_KEY_ID")); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_ISSUER")); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_AUDIENCE")); value != "" {
		c.WorkloadIdentity.Audience = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEMS_ROOT")); value != "" {
		c.Storage.ProblemsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SUBMISSIONS_ROOT")); value != "" {
		c.Storage.SubmissionsRoot = value
	}
	return nil
}

func applyPlatformBootstrapEnv(c *config.Config) error {
	if !strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") {
		return errors.New("platform bootstrap Gateway requires OJOS_ENVIRONMENT=production")
	}
	required := func(name string) (string, error) {
		value := strings.TrimSpace(os.Getenv(name))
		if value == "" {
			return "", fmt.Errorf("platform bootstrap Gateway requires %s", name)
		}
		return value, nil
	}
	var err error
	if c.Redis.Url, err = required("REDIS_URL"); err != nil {
		return err
	}
	if c.Jwt.Secret, err = required("JWT_SECRET"); err != nil {
		return err
	}
	if c.Orchestrator.Endpoint, err = required("ORCHESTRATOR_PLATFORM_ORIGIN"); err != nil {
		return err
	}
	if c.Orchestrator.InternalToken, err = required("ORCHESTRATOR_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ManagementToken, err = required("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ContributionAckToken, err = required("ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN"); err != nil {
		return err
	}
	c.Orchestrator.NodeID = strings.TrimSpace(os.Getenv("ORCHESTRATOR_NODE_ID"))
	if c.AuthService.Endpoint, err = required("AUTH_SERVICE_ENDPOINT"); err != nil {
		return err
	}
	if c.WorkloadIdentity.PublicKeyFile, err = required("OJOS_WORKLOAD_PUBLIC_KEY_FILE"); err != nil {
		return err
	}
	if c.WorkloadIdentity.KeyID, err = required("OJOS_WORKLOAD_KEY_ID"); err != nil {
		return err
	}
	if c.WorkloadIdentity.Issuer, err = required("OJOS_WORKLOAD_ISSUER"); err != nil {
		return err
	}
	if c.WorkloadIdentity.Audience, err = required("OJOS_WORKLOAD_AUDIENCE"); err != nil {
		return err
	}
	c.Jaeger.Endpoint = strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT"))
	// Auth is the only reserved static platform upstream. Every business route,
	// trusted service and status entry comes from an active Contribution revision.
	c.Proxy = config.ProxyConfig{
		TrustedServices: []config.ProxyTrustedServiceConfig{{
			ServiceID: "auth-service", Target: c.AuthService.Endpoint,
			StripPrefix: "/api", HealthCheckID: "auth-service-health",
		}},
		Routes: []config.ProxyRouteConfig{{
			Prefix: "/api/auth", Target: c.AuthService.Endpoint,
			StripPrefix: "/api", AuthMode: "optional", TimeoutMS: 30000,
		}},
	}
	c.ServiceStatus = config.ServiceStatusConfig{ComposeServices: []string{"auth-service", "gateway"}}
	c.Storage = config.StorageConfig{}
	c.Database = config.DatabaseConfig{}
	c.InternalAuth = config.InternalAuthConfig{}
	for name, value := range map[string]string{
		"JWT_SECRET":                                  c.Jwt.Secret,
		"ORCHESTRATOR_INTERNAL_TOKEN":                 c.Orchestrator.InternalToken,
		"ORCHESTRATOR_GATEWAY_ADMIN_TOKEN":            c.Orchestrator.ManagementToken,
		"ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
	} {
		if len(value) < 32 {
			return fmt.Errorf("platform bootstrap Gateway requires %s to be at least 32 bytes", name)
		}
	}
	if c.Orchestrator.ManagementToken == c.Jwt.Secret ||
		c.Orchestrator.ManagementToken == c.Orchestrator.InternalToken ||
		c.Orchestrator.ManagementToken == c.Orchestrator.ContributionAckToken {
		return errors.New("platform bootstrap Gateway management token must be distinct from JWT and outbound Orchestrator credentials")
	}
	return nil
}

func applyManagedEnv(c *config.Config) error {
	for _, name := range []string{
		"REDIS_URL", "JAEGER_ENDPOINT", "JWT_SECRET", "ORCHESTRATOR_ENDPOINT",
		"ORCHESTRATOR_INTERNAL_TOKEN", "CONTRIBUTION_ACK_TOKEN", "ORCHESTRATOR_NODE_ID", "AUTH_SERVICE_ENDPOINT",
		"OJOS_PROBLEMS_ROOT", "OJOS_SUBMISSIONS_ROOT",
	} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("managed Gateway rejects legacy configuration variable %s", name)
		}
	}
	// The image YAML remains a Compose/development fallback only. Managed
	// workloads discard every legacy address, token and static business route
	// before consuming compiler-generated Agent materialization.
	c.Redis = config.RedisConfig{Url: strings.TrimSpace(os.Getenv("OJOS_SECRET_REDIS_URL"))}
	c.Jwt = config.JwtConfig{Secret: strings.TrimSpace(os.Getenv("OJOS_SECRET_JWT_SECRET"))}
	c.Jaeger = config.JaegerConfig{Endpoint: strings.TrimSpace(os.Getenv("OJOS_CONFIG_TRACING_ENDPOINT"))}
	c.Orchestrator = config.OrchestratorConfig{
		Endpoint:             strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT")),
		InternalToken:        strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN")),
		ContributionAckToken: strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN")),
		NodeID:               strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_NODEID")),
	}
	c.WorkloadIdentity = config.WorkloadIdentityConfig{
		PublicKeyFile: strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE")),
		KeyID:         strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_KEY_ID")),
		Issuer:        strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_ISSUER")),
		Audience:      strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_AUDIENCE")),
	}
	c.Proxy = config.ProxyConfig{}
	c.ServiceStatus = config.ServiceStatusConfig{}
	c.AuthService = config.AuthServiceConfig{}
	c.Storage = config.StorageConfig{}
	c.Database = config.DatabaseConfig{}
	c.InternalAuth = config.InternalAuthConfig{}
	for name, value := range map[string]string{
		"redis.url":                         c.Redis.Url,
		"jwt.secret":                        c.Jwt.Secret,
		"orchestrator.endpoint":             c.Orchestrator.Endpoint,
		"orchestrator.internalToken":        c.Orchestrator.InternalToken,
		"orchestrator.contributionAckToken": c.Orchestrator.ContributionAckToken,
		"orchestrator.nodeId":               c.Orchestrator.NodeID,
		"workload.publicKeyFile":            c.WorkloadIdentity.PublicKeyFile,
		"workload.keyId":                    c.WorkloadIdentity.KeyID,
		"workload.issuer":                   c.WorkloadIdentity.Issuer,
		"workload.audience":                 c.WorkloadIdentity.Audience,
	} {
		if strings.TrimSpace(value) == "" {
			return fmt.Errorf("managed Gateway requires Agent materialization for %s", name)
		}
	}
	if len(c.Jwt.Secret) < 32 || len(c.Orchestrator.InternalToken) < 32 || len(c.Orchestrator.ContributionAckToken) < 32 {
		return errors.New("managed Gateway JWT and Orchestrator secrets must be at least 32 bytes")
	}
	return nil
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true")
}

func platformBootstrapEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_PLATFORM_BOOTSTRAP"))
	return value == "1" || strings.EqualFold(value, "true")
}

func productionModeEnabled() bool {
	return managedEnvironment() || platformBootstrapEnvironment() ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
}

func inferServiceID(target string) string {
	targetURL, err := url.Parse(strings.TrimSpace(target))
	if err != nil {
		return ""
	}
	return targetURL.Hostname()
}

func validateWorkloadIdentityConfig(c config.WorkloadIdentityConfig, production bool) error {
	if production && strings.TrimSpace(c.PublicKeyFile) == "" {
		return fmt.Errorf("production Gateway requires the workload identity public key")
	}
	return nil
}

func workloadIdentityVerifier(c config.WorkloadIdentityConfig) (*workload.Verifier, error) {
	return workload.NewVerifierFromPEMFile(c.PublicKeyFile, c.KeyID, c.Issuer, c.Audience)
}

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.Redis == nil || s.Redis.Ping(ctx).Err() != nil {
		return errors.New("Gateway projection Redis is unavailable")
	}
	if s.Orchestrator == nil || !s.Orchestrator.Configured() {
		return errors.New("Orchestrator control-plane client is unavailable")
	}
	s.contributionMu.Lock()
	contributionReady := s.contributionReady
	contributionError := s.contributionError
	s.contributionMu.Unlock()
	if !contributionReady {
		if contributionError == "" {
			contributionError = "snapshot has not been observed"
		}
		return fmt.Errorf("active Contribution projection is unavailable: %s", contributionError)
	}
	if !managedEnvironment() {
		return nil
	}
	if s.Context == nil || s.PermissionChecker == nil {
		return errors.New("managed Service Context permission binding is unavailable")
	}
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil {
		return fmt.Errorf("read managed Service Context: %w", err)
	}
	if err := snapshot.RequireService("gateway"); err != nil {
		return err
	}
	binding, err := snapshot.Binding(permissionBindingName)
	if err != nil || binding.APIID != sharedperm.DefaultPermissionCheckApiID {
		return errors.New("managed auth.user.permission.check binding is unavailable")
	}
	if _, err := snapshot.Client(); err != nil {
		return fmt.Errorf("configure managed permission client: %w", err)
	}
	return nil
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s == nil {
		return
	}
	s.contributionMu.Lock()
	cancel := s.contributionCancel
	done := s.contributionDone
	s.contributionCancel = nil
	s.contributionDone = nil
	s.contributionMu.Unlock()
	if cancel != nil {
		cancel()
	}
	if done != nil {
		select {
		case <-done:
		case <-ctx.Done():
		}
	}
	if s.ServiceProxy != nil {
		s.ServiceProxy.Close()
	}
	if s.Context != nil {
		_ = s.Context.Close()
	}

	if s.Redis != nil {
		_ = s.Redis.Close()
	}

	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}

	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
