// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/moduleregistry"
	"ojos-gateway/internal/proxy"
	"ojos-shared/security/internalauth"

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

	Proxy          http.HandlerFunc
	InternalSigner *internalauth.Signer
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

	if err := moduleregistry.BootstrapBuiltin(ctx, moduleregistry.NewRepository(db)); err != nil {
		log.Fatalf("bootstrap module registry failed: %v", err)
	}

	proxyHandler, err := proxy.NewConfigProxy(c.Proxy.Routes, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		log.Fatalf("init proxy failed: %v", err)
	}

	return &ServiceContext{
		Config:         c,
		Logger:         zlog,
		DB:             db,
		Redis:          redisClient,
		Tracer:         tp,
		Proxy:          proxyHandler,
		InternalSigner: internalSigner,
	}
}

func applyEnvOverrides(c *config.Config) {
	if value := firstEnv("DATABASE_URL", "POSTGRES_DSN"); value != "" {
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
	if value := strings.TrimSpace(os.Getenv("MODULE_INSTALLER_ENDPOINT")); value != "" {
		c.Installer.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("MODULE_INSTALLER_INTERNAL_TOKEN")); value != "" {
		c.Installer.InternalToken = value
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
