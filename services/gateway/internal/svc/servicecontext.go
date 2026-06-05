// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"net/http"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/proxy"
	"ojos-shared/security/internalauth"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider

	Proxy          http.HandlerFunc
	InternalSigner *internalauth.Signer
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()

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

	proxyHandler, err := proxy.NewConfigProxy(c.Proxy.Routes, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		log.Fatalf("init proxy failed: %v", err)
	}

	return &ServiceContext{
		Config:         c,
		Logger:         zlog,
		DB:             db,
		Tracer:         tp,
		Proxy:          proxyHandler,
		InternalSigner: internalSigner,
	}
}

func (s *ServiceContext) Close(ctx context.Context) {
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
