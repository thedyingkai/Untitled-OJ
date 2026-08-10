// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"
	"os"
	"strings"

	"ojos-user-service/internal/config"
	"ojos-user-service/internal/middleware"
	"ojos-user-service/internal/store"

	"ojos-shared/database"
	sharedperm "ojos-shared/security/permission"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/zeromicro/go-zero/rest"
)

type ServiceContext struct {
	Config       config.Config
	ProfileStore *store.ProfileStore
	DB           *pgxpool.Pool
	Permission   sharedperm.UserChecker

	UserContextMiddleware rest.Middleware
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)

	var db *pgxpool.Pool
	var err error
	if c.Database.Url != "" {
		db, err = database.NewPostgresPoolByURL(ctx, c.Database.Url)
		if err != nil {
			log.Fatalf("connect user database failed: %v", err)
		}
	}

	profileStore, err := store.NewProfileStore(c.Storage.ProfilesRoot, db)
	if err != nil {
		panic(err)
	}
	permissionChecker, err := sharedperm.NewManagedOrLegacyUserChecker(
		"user-service",
		sharedperm.DefaultPermissionCheckBinding,
		permissionCheckerConfig(c),
		db,
	)
	if err != nil {
		log.Fatalf("configure permission_check ApiBinding failed: %v", err)
	}
	return &ServiceContext{
		Config:                c,
		ProfileStore:          profileStore,
		DB:                    db,
		Permission:            permissionChecker,
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
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
	if value := firstEnv("USER_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_USER_DATA_DIR")); value != "" {
		c.Storage.ProfilesRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
	}
	if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
		c.AuthService.AdminToken = value
	}
	// Deliberately a dedicated variable rather than the generic
	// OJOS_INTERNAL_GATEWAY_ENDPOINT: switching the permission check onto the
	// gateway also requires a service credential and a service permission grant,
	// so it must be an explicit opt-in per deployment.
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
	if value := firstEnv("OJOS_USER_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.AuthService.ServiceToken = value
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
	_ = ctx
}
