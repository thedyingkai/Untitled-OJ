// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"strings"

	"ojos-gateway/internal/authclient"
	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	gtopology "ojos-gateway/internal/topologyprojection"
	"ojos-shared/security/internalauth"
	"ojos-shared/security/workload"

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
	TopologyProjection  *gtopology.Store
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)
	if err := validateWorkloadIdentityConfig(
		c.WorkloadIdentity,
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production"),
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
	serviceProxy, err := proxy.NewServiceProxy(c.Proxy.Routes, c.Proxy.TrustedServices, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		log.Fatalf("init proxy failed: %v", err)
	}
	serviceProxy.SetNodeID(c.Orchestrator.NodeID)
	if strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) != "" {
		workloadVerifier, verifyErr := workload.NewVerifierFromPEMFile(
			c.WorkloadIdentity.PublicKeyFile,
			c.WorkloadIdentity.KeyID,
			c.WorkloadIdentity.Issuer,
			c.WorkloadIdentity.Audience,
		)
		if verifyErr != nil {
			log.Fatalf("configure workload identity verifier failed: %v", verifyErr)
		}
		serviceProxy.SetWorkloadVerifier(workloadVerifier)
	}
	serviceProxy.SetPermissionChecker(func(ctx context.Context, authHeader string, caller proxy.PermissionCheckCaller, permissionCode string) (bool, error) {
		return authClient.HasSystemPermission(ctx, authHeader, authclient.PermissionCaller{
			Type:    caller.Type,
			UserID:  caller.UserID,
			Service: caller.Service,
			NodeID:  caller.NodeID,
			APIID:   caller.APIID,
		}, permissionCode)
	})
	routeTableOptions := routeTableOptionsFromConfig(c.Proxy)
	serviceStatusDriver := servicestatus.NewComposeDriver(routeTableOptions.TrustedServices, c.ServiceStatus.ComposeServices...)
	orchestratorClient := orchestratorsnapshot.NewClient(c.Orchestrator.Endpoint, c.Orchestrator.InternalToken)
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

	return &ServiceContext{
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
		TopologyProjection:  topologyProjection,
	}
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

func applyEnvOverrides(c *config.Config) {
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

func (s *ServiceContext) Close(ctx context.Context) {
	if s.ServiceProxy != nil {
		s.ServiceProxy.Close()
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
