// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Redis  *redis.Client
	Tracer *sdktrace.TracerProvider

	Proxy               http.HandlerFunc
	ServiceProxy        *proxy.ServiceProxy
	ServiceStatusDriver servicestatus.ServiceStatusDriver
	RouteTableOptions   servicestatus.RouteTableOptions
	InternalSigner      *internalauth.Signer
	Orchestrator        *orchestratorsnapshot.Client
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}

	db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
	if err != nil {
		log.Fatalf("connect postgres failed: %v", err)
	}

	redisOptions, err := redis.ParseURL(c.Redis.Url)
	if err != nil {
		log.Fatalf("parse redis url failed: %v", err)
	}
	redisClient := redis.NewClient(redisOptions)
	if err := redisClient.Ping(ctx).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
	}

	internalAuthCfg := internalauth.Config{
		Enabled:          c.InternalAuth.Enabled,
		RotationInterval: time.Duration(c.InternalAuth.RotationIntervalSeconds) * time.Second,
		VerifyGrace:      time.Duration(c.InternalAuth.VerifyGraceSeconds) * time.Second,
		RotateBefore:     time.Duration(c.InternalAuth.RotateBeforeSeconds) * time.Second,
		TimestampSkew:    time.Duration(c.InternalAuth.TimestampSkewSeconds) * time.Second,
		NonceTTL:         time.Duration(c.InternalAuth.NonceTTLSeconds) * time.Second,
	}

	var internalSigner *internalauth.Signer
	if c.InternalAuth.Enabled {
		internalKeyManager := internalauth.NewKeyManager(db, internalAuthCfg)
		internalSigner = internalauth.NewSigner(internalKeyManager)
	}

	serviceProxy, err := proxy.NewServiceProxy(c.Proxy.Routes, c.Proxy.TrustedServices, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		log.Fatalf("init proxy failed: %v", err)
	}
	serviceProxy.SetAdminChecker(func(ctx context.Context, userID int64) (bool, error) {
		return sharedperm.HasUserPermission(ctx, db, userID, "system.admin", sharedperm.SystemScope())
	})
	routeTableOptions := routeTableOptionsFromConfig(c.Proxy)
	serviceStatusDriver := servicestatus.NewComposeDriver(routeTableOptions.TrustedServices, c.ServiceStatus.ComposeServices...)
	orchestratorClient := orchestratorsnapshot.NewClient(c.Orchestrator.Endpoint, c.Orchestrator.InternalToken)
	var snapshot servicestatus.Snapshot
	if err := orchestratorClient.DecodeOrchestratorSnapshot(ctx, false, &snapshot); err == nil {
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
		DB:                  db,
		Redis:               redisClient,
		Tracer:              tp,
		Proxy:               serviceProxy.ServeHTTP,
		ServiceProxy:        serviceProxy,
		ServiceStatusDriver: serviceStatusDriver,
		RouteTableOptions:   routeTableOptions,
		InternalSigner:      internalSigner,
		Orchestrator:        orchestratorClient,
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
	if value := firstEnv("DATABASE_URL", "OJ_DATABASE_URL"); value != "" {
		c.Database.Url = value
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

func (s *ServiceContext) Close(ctx context.Context) {
	if s.DB != nil {
		s.DB.Close()
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
