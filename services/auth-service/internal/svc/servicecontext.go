// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"os"
	"strings"
	"sync"

	"ojos-auth-service/internal/config"
	authmw "ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/zeromicro/go-zero/rest"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider

	UserRepo    *repository.UserRepository
	AdminRepo   *repository.AdminRepository
	AuthService *service.AuthService

	AuthMiddleware rest.Middleware
	SmokeAuth      *SmokePermissionStore
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)
	smokeMode := smokeModeEnabled()

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}

	var db *pgxpool.Pool
	var userRepo *repository.UserRepository
	var adminRepo *repository.AdminRepository
	var smokeAuth *SmokePermissionStore
	if smokeMode {
		smokeAuth = NewSmokePermissionStore()
		smokeAuth.Allow("judge-api", "storage.object.read", "storage.object.write", "judge.submission.create", "judge.result.read")
		smokeAuth.Allow("judge-worker", "storage.object.read", "storage.object.write")
	} else {
		var err error
		db, err = database.NewPostgresPoolByURL(ctx, c.Database.Url)
		if err != nil {
			log.Fatalf("connect postgres failed: %v", err)
		}
		userRepo = repository.NewUserRepository(db)
		adminRepo = repository.NewAdminRepository(db)
	}

	authService := service.NewAuthService(
		userRepo,
		c.Jwt.Secret,
		c.Jwt.ExpireHours,
	)

	return &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		UserRepo:    userRepo,
		AdminRepo:   adminRepo,
		AuthService: authService,

		AuthMiddleware: authmw.NewAuthMiddleware(c.Jwt.Secret, c.InternalAuth.Token).Handle,
		SmokeAuth:      smokeAuth,
	}
}

func applyEnvOverrides(c *config.Config) {
	if value := firstEnv("AUTH_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_INTERNAL_TOKEN")); value != "" {
		c.InternalAuth.Token = value
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

func smokeModeEnabled() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_SMOKE_MODE"))
	return value == "1" || strings.EqualFold(value, "true")
}

type SmokePermissionStore struct {
	mu      sync.RWMutex
	allowed map[string]map[string]bool
}

func NewSmokePermissionStore() *SmokePermissionStore {
	return &SmokePermissionStore{allowed: map[string]map[string]bool{}}
}

func (s *SmokePermissionStore) Allow(service string, permissions ...string) {
	service = strings.TrimSpace(service)
	if service == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.allowed[service] == nil {
		s.allowed[service] = map[string]bool{}
	}
	for _, permission := range permissions {
		permission = strings.TrimSpace(permission)
		if permission != "" {
			s.allowed[service][permission] = true
		}
	}
}

func (s *SmokePermissionStore) ServiceCallerCanUsePermission(service string, permission string) bool {
	if s == nil {
		return false
	}
	service = strings.TrimSpace(service)
	permission = strings.TrimSpace(permission)
	if service == "" || permission == "" {
		return false
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.allowed[service] != nil && s.allowed[service][permission]
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
