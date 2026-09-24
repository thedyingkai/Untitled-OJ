// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"crypto/ed25519"
	"errors"
	"fmt"
	"strings"
	"time"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/contributionprojection"
	authmw "ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"
	atopology "ojos-auth-service/internal/topologyprojection"
	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/workload"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	"go.uber.org/zap"
)

func NewServiceContext(c config.Config) (result *ServiceContext, startupErr error) {
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		return nil, err
	}
	adminBootstrapSecret, adminBootstrapEnabled, err := resolveAdminBootstrapSecret(c.AdminBootstrap)
	if err != nil {
		return nil, fmt.Errorf("configure initial administrator bootstrap: %w", err)
	}
	defer clear(adminBootstrapSecret)
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

	if err := validateWorkloadIdentityConfig(c.WorkloadIdentity, productionModeEnabled()); err != nil {
		return nil, fmt.Errorf("configure workload identity: %w", err)
	}

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, fmt.Errorf("init logger: %w", err)
	}
	defer func() {
		if startupErr != nil {
			_ = zlog.Sync()
		}
	}()

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, fmt.Errorf("init tracing: %w", err)
	}
	defer func() {
		if startupErr != nil && tp != nil {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			_ = tp.Shutdown(shutdownCtx)
		}
	}()

	var db *pgxpool.Pool
	var userRepo *repository.UserRepository
	var adminRepo *repository.AdminRepository
	var adminBootstrap *service.AdminBootstrapService
	var contributionProjection *contributionprojection.Reconciler

	db, err = database.NewPostgresPoolByURL(ctx, c.Database.Url)
	if err != nil {
		return nil, errors.New("connect to claimed Auth PostgreSQL database")
	}
	defer func() {
		if startupErr != nil {
			db.Close()
		}
	}()
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

		AuthMiddleware: authMiddleware.Handle,
		PermissionReadMiddleware: authmw.NewPermissionReadMiddleware(
			c.Orchestrator.ManagementToken,
			authMiddleware.Handle,
		).Handle,
		DelegatedPermissionMiddleware: authMiddleware.HandleDelegated,
		WorkloadControlPlaneMiddleware: authmw.NewAuthMiddleware(
			c.Jwt.Secret,
			c.WorkloadIdentity.ControlPlaneToken,
		).Handle,
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
