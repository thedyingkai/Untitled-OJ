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
	serviceRouteAuthorizer := func(
		ctx context.Context,
		serviceCode string,
		credentialToken string,
		apiID string,
		permissionCode string,
	) (bool, error) {
		if smokeAuth != nil {
			return smokeAuth.ServiceCallerCanUsePermission(
				serviceCode,
				permissionCode,
				apiID,
				credentialToken,
			), nil
		}
		if adminRepo == nil {
			return false, nil
		}
		return adminRepo.ServiceCallerCanUsePermission(
			ctx,
			serviceCode,
			permissionCode,
			apiID,
			credentialToken,
		)
	}

	return &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		UserRepo:    userRepo,
		AdminRepo:   adminRepo,
		AuthService: authService,

		AuthMiddleware: authmw.NewAuthMiddleware(
			c.Jwt.Secret,
			c.InternalAuth.Token,
			serviceRouteAuthorizer,
		).Handle,
		SmokeAuth: smokeAuth,
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
	mu          sync.RWMutex
	registered  map[string]map[string]SmokePermission
	identities  map[string]bool
	credentials map[string]map[string]bool
	grants      map[string]map[string]map[string]bool
}

type SmokePermission struct {
	Code        string
	ServiceCode string
	Name        string
	Description string
}

func NewSmokePermissionStore() *SmokePermissionStore {
	return &SmokePermissionStore{
		registered:  map[string]map[string]SmokePermission{},
		identities:  map[string]bool{},
		credentials: map[string]map[string]bool{},
		grants:      map[string]map[string]map[string]bool{},
	}
}

func (s *SmokePermissionStore) Allow(service string, permissions ...string) {
	service = strings.TrimSpace(service)
	if service == "" {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.identities[service] = true
	if s.credentials[service] == nil {
		s.credentials[service] = map[string]bool{}
	}
	s.credentials[service][""] = true
	if s.grants[service] == nil {
		s.grants[service] = map[string]map[string]bool{}
	}
	for _, permission := range permissions {
		permission = strings.TrimSpace(permission)
		if permission != "" {
			if s.grants[service][""] == nil {
				s.grants[service][""] = map[string]bool{}
			}
			s.grants[service][""][permission] = true
		}
	}
}

func (s *SmokePermissionStore) ServiceCallerCanUsePermission(service string, permission string, apiID string, token string) bool {
	if s == nil {
		return false
	}
	service = strings.TrimSpace(service)
	permission = strings.TrimSpace(permission)
	apiID = strings.TrimSpace(apiID)
	if service == "" || permission == "" {
		return false
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	if !s.identities[service] {
		return false
	}
	if !s.serviceCredentialAllowedLocked(service, token) {
		return false
	}
	if s.grants[service] == nil {
		return false
	}
	if apiID != "" && s.grants[service][apiID] != nil && s.grants[service][apiID][permission] {
		return true
	}
	return s.grants[service][""] != nil && s.grants[service][""][permission]
}

type SmokeServiceIdentity struct {
	ServiceCode     string
	AllowedAPIs     []string
	Grants          []SmokeServiceIdentityGrant
	CredentialToken string
}

type SmokeServiceIdentityGrant struct {
	APIID          string
	PermissionCode string
}

func (s *SmokePermissionStore) RegisterServicePermissions(service string, permissions []SmokePermission, identity *SmokeServiceIdentity) []string {
	if s == nil {
		return nil
	}
	service = strings.TrimSpace(service)
	if service == "" {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.registered[service] == nil {
		s.registered[service] = map[string]SmokePermission{}
	}
	registered := make([]string, 0, len(permissions))
	for _, item := range permissions {
		code := strings.TrimSpace(item.Code)
		if code == "" {
			continue
		}
		item.Code = code
		item.ServiceCode = service
		s.registered[service][code] = item
		registered = append(registered, code)
	}
	if identity != nil {
		s.registerServiceIdentityLocked(service, identity)
	}
	return registered
}

func (s *SmokePermissionStore) DeleteServicePermissions(service string) int64 {
	if s == nil {
		return 0
	}
	service = strings.TrimSpace(service)
	if service == "" {
		return 0
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	deleted := int64(len(s.registered[service]))
	delete(s.registered, service)
	delete(s.identities, service)
	delete(s.credentials, service)
	delete(s.grants, service)
	return deleted
}

func (s *SmokePermissionStore) ListPermissions() []SmokePermission {
	if s == nil {
		return nil
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	items := make([]SmokePermission, 0)
	for _, byPermission := range s.registered {
		for _, item := range byPermission {
			items = append(items, item)
		}
	}
	return items
}

func (s *SmokePermissionStore) registerServiceIdentityLocked(service string, identity *SmokeServiceIdentity) {
	identityService := strings.TrimSpace(identity.ServiceCode)
	if identityService == "" {
		identityService = service
	}
	if identityService != service {
		return
	}
	s.identities[service] = true
	if s.credentials[service] == nil {
		s.credentials[service] = map[string]bool{}
	}
	if token := strings.TrimSpace(identity.CredentialToken); token != "" {
		s.credentials[service][token] = true
	}
	s.grants[service] = map[string]map[string]bool{}
	allowed := map[string]bool{}
	for _, rawAPI := range identity.AllowedAPIs {
		apiID := strings.TrimSpace(rawAPI)
		if apiID != "" {
			allowed[apiID] = true
		}
	}
	for _, grant := range identity.Grants {
		apiID := strings.TrimSpace(grant.APIID)
		permission := strings.TrimSpace(grant.PermissionCode)
		if apiID == "" || permission == "" {
			continue
		}
		if len(allowed) > 0 && !allowed[apiID] {
			continue
		}
		if s.grants[service][apiID] == nil {
			s.grants[service][apiID] = map[string]bool{}
		}
		s.grants[service][apiID][permission] = true
	}
}

func (s *SmokePermissionStore) serviceCredentialAllowedLocked(service string, token string) bool {
	credentials := s.credentials[service]
	if len(credentials) == 0 {
		return false
	}
	token = strings.TrimSpace(token)
	if credentials[token] {
		return true
	}
	return credentials[""]
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
