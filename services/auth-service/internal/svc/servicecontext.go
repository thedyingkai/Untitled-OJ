// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"crypto/ed25519"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/contributionprojection"
	authmw "ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"
	atopology "ojos-auth-service/internal/topologyprojection"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/resourceoutput"
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
	DelegatedPermissionMiddleware  rest.Middleware
	WorkloadControlPlaneMiddleware rest.Middleware
	SmokeAuth                      *SmokePermissionStore
	WorkloadIssuer                 *workload.Issuer
	TopologyProjection             *atopology.Store
	ContributionProjection         *contributionprojection.Reconciler
}

const defaultAuthResourceFile = "/run/ojos/resources/auth/dsn"

func NewServiceContext(c config.Config) (*ServiceContext, error) {
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		return nil, err
	}
	smokeMode := smokeModeEnabled()
	adminBootstrapSecret, adminBootstrapEnabled, err := resolveAdminBootstrapSecret(c.AdminBootstrap)
	if err != nil {
		return nil, fmt.Errorf("configure initial administrator bootstrap: %w", err)
	}
	if adminBootstrapEnabled {
		if err := validateAdminBootstrapSecretSeparation(adminBootstrapSecret, map[string]string{
			"Jwt.Secret":                         c.Jwt.Secret,
			"InternalAuth.Token":                 c.InternalAuth.Token,
			"WorkloadIdentity.ControlPlaneToken": c.WorkloadIdentity.ControlPlaneToken,
			"Orchestrator.InternalToken":         c.Orchestrator.InternalToken,
			"Orchestrator.ManagementToken":       c.Orchestrator.ManagementToken,
			"Orchestrator.ContributionAckToken":  c.Orchestrator.ContributionAckToken,
		}); err != nil {
			return nil, fmt.Errorf("configure initial administrator bootstrap: %w", err)
		}
	}
	// Do not retain an inline clear-text bootstrap secret in Config after the
	// verifier has derived its digest.
	c.AdminBootstrap.Secret = ""
	if adminBootstrapEnabled && smokeMode {
		return nil, errors.New("initial administrator bootstrap requires PostgreSQL and is unavailable in smoke mode")
	}
	if err := validateWorkloadIdentityConfig(c.WorkloadIdentity, productionModeEnabled()); err != nil {
		return nil, fmt.Errorf("configure workload identity: %w", err)
	}

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, fmt.Errorf("init logger: %w", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, fmt.Errorf("init tracing: %w", err)
	}

	var db *pgxpool.Pool
	var userRepo *repository.UserRepository
	var adminRepo *repository.AdminRepository
	var adminBootstrap *service.AdminBootstrapService
	var smokeAuth *SmokePermissionStore
	var contributionProjection *contributionprojection.Reconciler
	if smokeMode {
		smokeAuth = NewSmokePermissionStore()
	} else {
		var err error
		db, err = database.NewPostgresPoolByURL(ctx, c.Database.Url)
		if err != nil {
			return nil, errors.New("connect to claimed Auth PostgreSQL database")
		}
		userRepo = repository.NewUserRepository(db)
		adminRepo = repository.NewAdminRepository(db)
		contributionProjection, err = contributionprojection.New(
			c.Orchestrator.Endpoint,
			c.Orchestrator.InternalToken,
			c.Orchestrator.ContributionAckToken,
			adminRepo,
		)
		if err != nil {
			return nil, fmt.Errorf("configure Contribution permission projection: %w", err)
		}
		if adminBootstrapEnabled {
			adminBootstrapRepo := repository.NewAdminBootstrapRepository(db)
			if err := adminBootstrapRepo.ValidateState(ctx); err != nil {
				return nil, fmt.Errorf("validate initial administrator bootstrap state: %w", err)
			}
			adminBootstrap, err = service.NewAdminBootstrapService(
				adminBootstrapRepo,
				adminBootstrapSecret,
			)
			if err != nil {
				return nil, fmt.Errorf("configure initial administrator bootstrap: %w", err)
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
	if strings.TrimSpace(c.WorkloadIdentity.PrivateKeyFile) != "" || strings.TrimSpace(c.WorkloadIdentity.PrivateKeyPEM) != "" {
		ttl := time.Duration(c.WorkloadIdentity.TTLSeconds) * time.Second
		if ttl <= 0 {
			ttl = workload.DefaultTTL
		}
		if strings.TrimSpace(c.WorkloadIdentity.PrivateKeyPEM) != "" {
			var privateKey ed25519.PrivateKey
			privateKey, err = parseWorkloadPrivateKeyPEM(c.WorkloadIdentity.PrivateKeyPEM)
			if err == nil {
				workloadIssuer, err = workload.NewIssuer(privateKey, c.WorkloadIdentity.KeyID, c.WorkloadIdentity.Issuer, c.WorkloadIdentity.Audience, ttl)
			}
		} else {
			workloadIssuer, err = workload.NewIssuerFromPEMFile(c.WorkloadIdentity.PrivateKeyFile, c.WorkloadIdentity.KeyID, c.WorkloadIdentity.Issuer, c.WorkloadIdentity.Audience, ttl)
		}
		if err != nil {
			return nil, fmt.Errorf("configure workload identity issuer: %w", err)
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

	serviceContext := &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		UserRepo:       userRepo,
		AdminRepo:      adminRepo,
		AuthService:    authService,
		AdminBootstrap: adminBootstrap,

		AuthMiddleware:                authMiddleware.Handle,
		DelegatedPermissionMiddleware: authMiddleware.HandleDelegated,
		WorkloadControlPlaneMiddleware: authmw.NewAuthMiddleware(
			c.Jwt.Secret,
			c.WorkloadIdentity.ControlPlaneToken,
		).Handle,
		SmokeAuth:              smokeAuth,
		WorkloadIssuer:         workloadIssuer,
		TopologyProjection:     topologyProjection,
		ContributionProjection: contributionProjection,
	}
	if contributionProjection != nil {
		initialCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		if err := contributionProjection.Reconcile(initialCtx); err != nil {
			zlog.Warn("initial Contribution permission projection failed; retaining the last durable snapshot", zap.Error(err))
		}
		cancel()
		contributionProjection.Start(5*time.Second, func(err error) {
			zlog.Warn("Contribution permission projection failed; retaining the last durable snapshot", zap.Error(err))
		})
	}
	return serviceContext, nil
}

func parseWorkloadPrivateKeyPEM(value string) (ed25519.PrivateKey, error) {
	block, _ := pem.Decode([]byte(strings.TrimSpace(value)))
	if block == nil {
		return nil, errors.New("workload private key is not PEM")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, errors.New("parse workload private key")
	}
	key, ok := parsed.(ed25519.PrivateKey)
	if !ok {
		return nil, errors.New("workload private key is not Ed25519")
	}
	return key, nil
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

func applyEnvOverrides(c *config.Config) error {
	managed := managedEnvironment()
	bootstrap := platformBootstrapEnvironment()
	production := productionModeEnabled()
	if managed && bootstrap {
		return errors.New("Auth cannot be both Agent-managed and a platform bootstrap service")
	}
	if managed {
		// The checked-in YAML is an unmanaged development default. Managed
		// workloads reconstruct every sensitive/runtime-bound field exclusively
		// from Agent materialization so a legacy value cannot silently survive.
		clearManagedRuntimeFields(c)
		path := firstEnv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultAuthResourceFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load Auth resource output: %w", err)
		}
		c.Database.Url = dsn
	} else if bootstrap {
		if !strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") {
			return errors.New("platform bootstrap Auth requires OJOS_ENVIRONMENT=production")
		}
		if err := rejectPlatformBootstrapMaterializationAliases(); err != nil {
			return err
		}
		clearManagedRuntimeFields(c)
		if err := applyPlatformBootstrapEnv(c); err != nil {
			return err
		}
	} else if production {
		return errors.New("production Auth requires OJOS_MANAGED_WORKLOAD=1 or OJOS_PLATFORM_BOOTSTRAP=1")
	} else if value := firstEnv("AUTH_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if !managed && !bootstrap && !production {
		applyDevelopmentEnvOverrides(c)
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_JWT_EXPIREHOURS")); value != "" {
		hours, err := strconv.Atoi(value)
		if err != nil || hours < 1 || hours > 168 {
			return errors.New("OJOS_CONFIG_JWT_EXPIREHOURS is invalid")
		}
		c.Jwt.ExpireHours = hours
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_MANAGEMENT_TOKEN")); value != "" {
		c.InternalAuth.Token = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ADMINBOOTSTRAP_SECRET")); value != "" {
		c.AdminBootstrap.Secret = value
		c.AdminBootstrap.SecretFile = ""
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_WORKLOAD_PRIVATEKEYPEM")); value != "" {
		c.WorkloadIdentity.PrivateKeyPEM = value
		c.WorkloadIdentity.PrivateKeyFile = ""
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_WORKLOAD_CONTROLPLANETOKEN")); value != "" {
		c.WorkloadIdentity.ControlPlaneToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_KEYID")); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_ISSUER")); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_AUDIENCE")); value != "" {
		c.WorkloadIdentity.Audience = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_TTLSECONDS")); value != "" {
		ttl, err := strconv.ParseInt(value, 10, 64)
		if err != nil || ttl != int64(workload.DefaultTTL/time.Second) {
			return errors.New("OJOS_CONFIG_WORKLOAD_TTLSECONDS must be 900")
		}
		c.WorkloadIdentity.TTLSeconds = ttl
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT")); value != "" {
		c.Orchestrator.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN")); value != "" {
		c.Orchestrator.InternalToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN")); value != "" {
		c.Orchestrator.ContributionAckToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_TRACING_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if managed {
		if c.Jwt.Secret == "" || c.InternalAuth.Token == "" || c.WorkloadIdentity.PrivateKeyPEM == "" || c.WorkloadIdentity.ControlPlaneToken == "" {
			return errors.New("managed Auth requires Agent-materialized JWT, management, and workload identity secrets")
		}
		if c.Orchestrator.Endpoint == "" || c.Orchestrator.InternalToken == "" || c.Orchestrator.ContributionAckToken == "" {
			return errors.New("managed Auth requires Agent-materialized Orchestrator projection configuration")
		}
	}
	if bootstrap {
		if c.Database.Url == "" || c.Jwt.Secret == "" || c.InternalAuth.Token == "" || c.WorkloadIdentity.PrivateKeyFile == "" || c.WorkloadIdentity.ControlPlaneToken == "" {
			return errors.New("platform bootstrap Auth requires explicit database, JWT, management, and workload identity configuration")
		}
		if c.Orchestrator.Endpoint == "" || c.Orchestrator.InternalToken == "" || c.Orchestrator.ContributionAckToken == "" {
			return errors.New("platform bootstrap Auth requires explicit Orchestrator projection configuration")
		}
	}
	return nil
}

// rejectPlatformBootstrapMaterializationAliases keeps the platform bootstrap
// configuration a closed set. These variables are generated for Agent-managed
// workloads and are intentionally applied after the resource output is loaded.
// Accepting one in bootstrap mode would let it override a value that
// applyPlatformBootstrapEnv already validated (including token separation), or
// re-enable the one-time administrator route with an inline secret.
func rejectPlatformBootstrapMaterializationAliases() error {
	for _, name := range []string{
		"AUTH_ADMIN_BOOTSTRAP_SECRET",
		"OJOS_SECRET_JWT_SECRET",
		"OJOS_CONFIG_JWT_EXPIREHOURS",
		"OJOS_SECRET_MANAGEMENT_TOKEN",
		"OJOS_SECRET_ADMINBOOTSTRAP_SECRET",
		"OJOS_SECRET_WORKLOAD_PRIVATEKEYPEM",
		"OJOS_SECRET_WORKLOAD_CONTROLPLANETOKEN",
		"OJOS_CONFIG_WORKLOAD_KEYID",
		"OJOS_CONFIG_WORKLOAD_ISSUER",
		"OJOS_CONFIG_WORKLOAD_AUDIENCE",
		"OJOS_CONFIG_WORKLOAD_TTLSECONDS",
		"OJOS_CONFIG_ORCHESTRATOR_ENDPOINT",
		"OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN",
		"OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN",
		"OJOS_CONFIG_TRACING_ENDPOINT",
	} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("platform bootstrap Auth forbids Agent materialization variable %s", name)
		}
	}
	return nil
}

// applyPlatformBootstrapEnv is intentionally a closed list.  The platform
// Auth instance is production infrastructure, but it is not an Agent-managed
// workload and therefore must not read /run/ojos resource/context material.
// Clearing the image YAML before this function prevents development defaults
// from becoming a second production configuration truth.
func applyPlatformBootstrapEnv(c *config.Config) error {
	required := func(name string) (string, error) {
		value := strings.TrimSpace(os.Getenv(name))
		if value == "" {
			return "", fmt.Errorf("platform bootstrap Auth requires %s", name)
		}
		return value, nil
	}
	var err error
	if c.Database.Url, err = required("AUTH_DATABASE_URL"); err != nil {
		return err
	}
	if c.Jwt.Secret, err = required("JWT_SECRET"); err != nil {
		return err
	}
	if c.InternalAuth.Token, err = required("AUTH_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.WorkloadIdentity.PrivateKeyFile, err = required("OJOS_WORKLOAD_PRIVATE_KEY_FILE"); err != nil {
		return err
	}
	if c.WorkloadIdentity.ControlPlaneToken, err = required("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"); err != nil {
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
	c.WorkloadIdentity.TTLSeconds = int64(workload.DefaultTTL / time.Second)
	if c.Orchestrator.Endpoint, err = required("ORCHESTRATOR_PLATFORM_ORIGIN"); err != nil {
		return err
	}
	if c.Orchestrator.InternalToken, err = required("ORCHESTRATOR_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ManagementToken, err = required("ORCHESTRATOR_AUTH_ADMIN_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ContributionAckToken, err = required("ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN"); err != nil {
		return err
	}
	c.Jaeger.Endpoint = strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT"))
	if file := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET_FILE")); file != "" {
		c.AdminBootstrap.SecretFile = file
	}
	for name, value := range map[string]string{
		"JWT_SECRET":                               c.Jwt.Secret,
		"AUTH_INTERNAL_TOKEN":                      c.InternalAuth.Token,
		"ORCHESTRATOR_AUTH_WORKLOAD_TOKEN":         c.WorkloadIdentity.ControlPlaneToken,
		"ORCHESTRATOR_INTERNAL_TOKEN":              c.Orchestrator.InternalToken,
		"ORCHESTRATOR_AUTH_ADMIN_TOKEN":            c.Orchestrator.ManagementToken,
		"ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
	} {
		if len(value) < 32 {
			return fmt.Errorf("platform bootstrap Auth requires %s to be at least 32 bytes", name)
		}
	}
	for name, value := range map[string]string{
		"JWT_SECRET":                               c.Jwt.Secret,
		"AUTH_INTERNAL_TOKEN":                      c.InternalAuth.Token,
		"ORCHESTRATOR_INTERNAL_TOKEN":              c.Orchestrator.InternalToken,
		"ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
		"ORCHESTRATOR_AUTH_WORKLOAD_TOKEN":         c.WorkloadIdentity.ControlPlaneToken,
	} {
		if c.Orchestrator.ManagementToken == value {
			return fmt.Errorf("platform bootstrap Auth requires ORCHESTRATOR_AUTH_ADMIN_TOKEN to be distinct from %s", name)
		}
	}
	return nil
}

func clearManagedRuntimeFields(c *config.Config) {
	c.Database.Url = ""
	c.Jwt.Secret = ""
	c.InternalAuth.Token = ""
	c.AdminBootstrap.Secret = ""
	c.AdminBootstrap.SecretFile = ""
	c.WorkloadIdentity.PrivateKeyFile = ""
	c.WorkloadIdentity.PrivateKeyPEM = ""
	c.WorkloadIdentity.ControlPlaneToken = ""
	c.Orchestrator.Endpoint = ""
	c.Orchestrator.InternalToken = ""
	c.Orchestrator.ManagementToken = ""
	c.Orchestrator.ContributionAckToken = ""
	c.Jaeger.Endpoint = ""
}

func applyDevelopmentEnvOverrides(c *config.Config) {
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
			log.Printf("ignoring invalid OJOS_WORKLOAD_TTL_SECONDS")
		} else {
			c.WorkloadIdentity.TTLSeconds = ttl
		}
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
	return managedEnvironment() || platformBootstrapEnvironment() ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true")

}

func platformBootstrapEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_PLATFORM_BOOTSTRAP"))
	return value == "1" || strings.EqualFold(value, "true")
}

func validateWorkloadIdentityConfig(c config.WorkloadIdentityConfig, production bool) error {
	keyConfigured := strings.TrimSpace(c.PrivateKeyFile) != "" || strings.TrimSpace(c.PrivateKeyPEM) != ""
	if strings.TrimSpace(c.PrivateKeyFile) != "" && strings.TrimSpace(c.PrivateKeyPEM) != "" {
		return fmt.Errorf("private key file and inline PEM are mutually exclusive")
	}
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

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil {
		return errors.New("claimed Auth PostgreSQL database is unavailable")
	}
	if err := s.DB.Ping(ctx); err != nil {
		return errors.New("claimed Auth PostgreSQL database is unavailable")
	}
	if productionModeEnabled() {
		if s.WorkloadIssuer == nil || s.TopologyProjection == nil || s.ContributionProjection == nil {
			return errors.New("managed Auth control-plane projection is unavailable")
		}
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
	if s.ContributionProjection != nil {
		s.ContributionProjection.Close()
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
