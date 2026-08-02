// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"os"
	"strings"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/middleware"
	"ojos-problem-service/internal/repository"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider
	Redis  *redis.Client

	Repo       *repository.Repository
	Permission sharedperm.UserChecker

	InternalAuthMiddleware rest.Middleware
	UserContextMiddleware  rest.Middleware
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
		Enabled:       c.InternalAuth.Enabled,
		TimestampSkew: time.Duration(c.InternalAuth.TimestampSkewSeconds) * time.Second,
		NonceTTL:      time.Duration(c.InternalAuth.NonceTTLSeconds) * time.Second,
	}

	var internalVerifier *internalauth.Verifier
	if c.InternalAuth.Enabled {
		internalKeyManager := internalauth.NewKeyManager(db, internalAuthCfg)
		internalNonceStore := internalauth.RedisNonceStore{
			Client: redisClient,
			Prefix: "ojos:internal-auth:nonce:",
		}

		internalVerifier = internalauth.NewVerifier(
			internalKeyManager,
			internalNonceStore,
			internalAuthCfg,
		)
	}

	return &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,
		Redis:  redisClient,

		Repo: repository.New(db),
		Permission: sharedperm.NewUserCheckerWithConfig(
			permissionCheckerConfig(c),
			db,
		),

		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
}

// permissionCheckerConfig keeps the routing decision in one place: gateway +
// api_id first, direct auth-service address only as a fallback.
func permissionCheckerConfig(c config.Config) sharedperm.RemoteCheckerConfig {
	return sharedperm.RemoteCheckerConfig{
		InternalGatewayEndpoint: c.AuthService.InternalGatewayEndpoint,
		ApiID:                   c.AuthService.PermissionCheckApiID,
		CallerService:           c.AuthService.CallerService,
		CallerNodeID:            c.AuthService.CallerNodeID,
		ServiceToken:            c.AuthService.ServiceToken,
		AuthServiceEndpoint:     c.AuthService.Endpoint,
		AuthServiceAdminToken:   c.AuthService.AdminToken,
	}
}

func applyEnvOverrides(c *config.Config) {
	if value := firstEnv("PROBLEM_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
		c.Redis.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
	}
	if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
		c.AuthService.AdminToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEMS_ROOT")); value != "" {
		c.Storage.ProblemsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_URL")); value != "" {
		c.Storage.ServiceEndpoint = value
	}
	if value := firstEnv("OJOS_INTERNAL_GATEWAY_ENDPOINT", "OJOS_INTERNAL_GATEWAY_URL"); value != "" {
		c.Storage.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_STORAGE_BUCKET")); value != "" {
		c.Storage.Bucket = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.Storage.ServiceToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.Storage.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.Storage.CallerNodeID = value
	}
	// Deliberately a dedicated variable rather than reusing
	// OJOS_INTERNAL_GATEWAY_ENDPOINT / OJOS_INTERNAL_GATEWAY_URL (which already
	// drive the storage client): switching the permission check onto the gateway
	// also requires a service credential and a service permission grant, so it
	// must be an explicit opt-in per deployment.
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT")); value != "" {
		c.AuthService.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CHECK_API_ID")); value != "" {
		c.AuthService.PermissionCheckApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.AuthService.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.AuthService.CallerNodeID = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.AuthService.ServiceToken = value
	}
}

func (s *ServiceContext) ActivePermissionChecker() sharedperm.UserChecker {
	if s == nil {
		return nil
	}
	if s.Permission != nil {
		return s.Permission
	}
	return sharedperm.NewDatabaseUserChecker(s.DB)
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.Redis != nil {
		_ = s.Redis.Close()
	}

	if s.DB != nil {
		s.DB.Close()
	}

	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}

	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
