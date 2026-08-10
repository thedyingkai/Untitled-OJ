// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"ojos-auth-service/internal/config"
	authmw "ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"
	atopology "ojos-auth-service/internal/topologyprojection"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/workload"
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

	UserRepo       *repository.UserRepository
	AdminRepo      *repository.AdminRepository
	AuthService    *service.AuthService
	AdminBootstrap *service.AdminBootstrapService

	AuthMiddleware                 rest.Middleware
	WorkloadControlPlaneMiddleware rest.Middleware
	SmokeAuth                      *SmokePermissionStore
	WorkloadIssuer                 *workload.Issuer
	TopologyProjection             *atopology.Store
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)
	smokeMode := smokeModeEnabled()
	adminBootstrapSecret, adminBootstrapEnabled, err := resolveAdminBootstrapSecret(c.AdminBootstrap)
	if err != nil {
		log.Fatalf("configure initial administrator bootstrap: %v", err)
	}
	if adminBootstrapEnabled {
		if err := validateAdminBootstrapSecretSeparation(adminBootstrapSecret, map[string]string{
			"Jwt.Secret":                         c.Jwt.Secret,
			"InternalAuth.Token":                 c.InternalAuth.Token,
			"WorkloadIdentity.ControlPlaneToken": c.WorkloadIdentity.ControlPlaneToken,
		}); err != nil {
			log.Fatalf("configure initial administrator bootstrap: %v", err)
		}
	}
	// Do not retain an inline clear-text bootstrap secret in Config after the
	// verifier has derived its digest.
	c.AdminBootstrap.Secret = ""
	if adminBootstrapEnabled && smokeMode {
		log.Fatalf("initial administrator bootstrap requires PostgreSQL and is unavailable in smoke mode")
	}
	if err := validateWorkloadIdentityConfig(c.WorkloadIdentity, productionModeEnabled()); err != nil {
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

	var db *pgxpool.Pool
	var userRepo *repository.UserRepository
	var adminRepo *repository.AdminRepository
	var adminBootstrap *service.AdminBootstrapService
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
		if adminBootstrapEnabled {
			adminBootstrapRepo := repository.NewAdminBootstrapRepository(db)
			if err := adminBootstrapRepo.ValidateState(ctx); err != nil {
				log.Fatalf("validate initial administrator bootstrap state: %v", err)
			}
			adminBootstrap, err = service.NewAdminBootstrapService(
				adminBootstrapRepo,
				adminBootstrapSecret,
			)
			if err != nil {
				log.Fatalf("configure initial administrator bootstrap: %v", err)
			}
		}
	}
	for index := range adminBootstrapSecret {
		adminBootstrapSecret[index] = 0
	}

	authService := service.NewAuthService(
		userRepo,
		c.Jwt.Secret,
		c.Jwt.ExpireHours,
	)
	var workloadIssuer *workload.Issuer
	if strings.TrimSpace(c.WorkloadIdentity.PrivateKeyFile) != "" {
		ttl := time.Duration(c.WorkloadIdentity.TTLSeconds) * time.Second
		if ttl <= 0 {
			ttl = workload.DefaultTTL
		}
		workloadIssuer, err = workload.NewIssuerFromPEMFile(
			c.WorkloadIdentity.PrivateKeyFile,
			c.WorkloadIdentity.KeyID,
			c.WorkloadIdentity.Issuer,
			c.WorkloadIdentity.Audience,
			ttl,
		)
		if err != nil {
			log.Fatalf("configure workload identity issuer failed: %v", err)
		}
	}
	legacyServiceRouteAuthorizer := func(
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
	topologyProjection := atopology.NewStore(db)
	serviceRouteAuthorizer := newServiceRouteAuthorizer(
		productionModeEnabled(),
		workloadIssuer.Verifier(),
		topologyProjection,
		legacyServiceRouteAuthorizer,
	)
	authMiddleware := authmw.NewAuthMiddleware(
		c.Jwt.Secret,
		c.InternalAuth.Token,
		serviceRouteAuthorizer,
	)
	if productionModeEnabled() {
		authMiddleware = authmw.NewStrictWorkloadAuthMiddleware(
			c.Jwt.Secret,
			c.InternalAuth.Token,
			serviceRouteAuthorizer,
		)
	}

	return &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		UserRepo:       userRepo,
		AdminRepo:      adminRepo,
		AuthService:    authService,
		AdminBootstrap: adminBootstrap,

		AuthMiddleware: authMiddleware.Handle,
		WorkloadControlPlaneMiddleware: authmw.NewAuthMiddleware(
			c.Jwt.Secret,
			c.WorkloadIdentity.ControlPlaneToken,
		).Handle,
		SmokeAuth:          smokeAuth,
		WorkloadIssuer:     workloadIssuer,
		TopologyProjection: topologyProjection,
	}
}

func newServiceRouteAuthorizer(
	production bool,
	verifier *workload.Verifier,
	projection *atopology.Store,
	legacy authmw.ServiceRouteAuthorizer,
) authmw.ServiceRouteAuthorizer {
	return func(
		ctx context.Context,
		serviceCode string,
		credentialToken string,
		apiID string,
		permissionCode string,
	) (bool, error) {
		if verifier != nil && projection != nil {
			claims, err := verifier.Verify(credentialToken, time.Now())
			if err == nil {
				if strings.TrimSpace(serviceCode) != claims.ServiceID {
					return false, nil
				}
				return projection.AuthorizeWorkload(
					ctx,
					claims.DeploymentID,
					claims.ServiceID,
					claims.NodeID,
					claims.CredentialGeneration,
					apiID,
					permissionCode,
				)
			}
		}
		if production || legacy == nil {
			return false, nil
		}
		return legacy(ctx, serviceCode, credentialToken, apiID, permissionCode)
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
	if value := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET")); value != "" {
		c.AdminBootstrap.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET_FILE")); value != "" {
		c.AdminBootstrap.SecretFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PRIVATE_KEY_FILE")); value != "" {
		c.WorkloadIdentity.PrivateKeyFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_CONTROL_PLANE_TOKEN")); value != "" {
		c.WorkloadIdentity.ControlPlaneToken = value
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
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_TTL_SECONDS")); value != "" {
		ttl, err := strconv.ParseInt(value, 10, 64)
		if err != nil || ttl <= 0 || ttl > 3600 {
			log.Fatalf("OJOS_WORKLOAD_TTL_SECONDS must be an integer from 1 through 3600")
		}
		c.WorkloadIdentity.TTLSeconds = ttl
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

func productionModeEnabled() bool {
	return strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func validateWorkloadIdentityConfig(c config.WorkloadIdentityConfig, production bool) error {
	keyConfigured := strings.TrimSpace(c.PrivateKeyFile) != ""
	controlPlaneConfigured := strings.TrimSpace(c.ControlPlaneToken) != ""
	if keyConfigured != controlPlaneConfigured {
		return fmt.Errorf("private key and dedicated control-plane token must be configured together")
	}
	if production && !keyConfigured {
		return fmt.Errorf("production Auth requires a private key and dedicated control-plane token")
	}
	expectedTTL := int64(workload.DefaultTTL / time.Second)
	if production && c.TTLSeconds != expectedTTL {
		return fmt.Errorf("production workload identity TTL must be %d seconds", expectedTTL)
	}
	return nil
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
