// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"

	"ojos-user-service/internal/config"
	"ojos-user-service/internal/middleware"
	"ojos-user-service/internal/store"

	"ojos-shared/database"
	"ojos-shared/resourceoutput"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/zeromicro/go-zero/rest"
)

type ServiceContext struct {
	Config       config.Config
	ProfileStore *store.ProfileStore
	DB           *pgxpool.Pool
	Permission   sharedperm.UserChecker
	Context      *servicecontext.ContextProvider
	Managed      bool

	UserContextMiddleware rest.Middleware
}

const (
	permissionBindingName       = sharedperm.DefaultPermissionCheckApiID
	defaultProfilesResourceFile = "/run/ojos/resources/profiles/dsn"
)

func NewServiceContext(c config.Config) (*ServiceContext, error) {
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		return nil, err
	}

	var db *pgxpool.Pool
	var err error
	if c.Database.Url != "" {
		db, err = database.NewPostgresPoolByURL(ctx, c.Database.Url)
		if err != nil {
			return nil, errors.New("connect to claimed user PostgreSQL database")
		}
	}
	if managedEnvironment() && db == nil {
		return nil, errors.New("managed user-service requires its claimed PostgreSQL database")
	}

	profileStore, err := store.NewProfileStore(c.Storage.ProfilesRoot, db)
	if err != nil {
		if db != nil {
			db.Close()
		}
		return nil, fmt.Errorf("configure user profile store: %w", err)
	}

	var contextProvider *servicecontext.ContextProvider
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		if db != nil {
			db.Close()
		}
		return nil, fmt.Errorf("load managed service context: %w", err)
	}
	var permissionChecker sharedperm.UserChecker
	if contextValue != nil {
		if err := contextValue.RequireService("user-service"); err != nil {
			if db != nil {
				db.Close()
			}
			return nil, err
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			_ = contextProvider.Close()
			if db != nil {
				db.Close()
			}
			return nil, fmt.Errorf("configure permission ApiBinding: %w", err)
		}
	} else {
		if managedEnvironment() {
			if db != nil {
				db.Close()
			}
			return nil, errors.New("managed user-service requires an Agent service context")
		}
		permissionChecker = sharedperm.NewUserCheckerWithConfig(permissionCheckerConfig(c), db)
		if permissionChecker == nil {
			if db != nil {
				db.Close()
			}
			return nil, errors.New("permission checker is not configured")
		}
	}
	return &ServiceContext{
		Config:                c,
		ProfileStore:          profileStore,
		DB:                    db,
		Permission:            permissionChecker,
		Context:               contextProvider,
		Managed:               managedEnvironment(),
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}, nil
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

func applyEnvOverrides(c *config.Config) error {
	if managedEnvironment() {
		path := firstEnv("OJOS_RESOURCE_PROFILES_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultProfilesResourceFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load profiles resource output: %w", err)
		}
		c.Database.Url = dsn
	} else if value := firstEnv("USER_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
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
	return nil
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true") ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if err := s.DB.Ping(ctx); err != nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if s.Context == nil {
		if managedEnvironment() {
			return errors.New("managed service context is unavailable")
		}
		return nil
	}
	// Invalid or partial replacements retain the last-known-good snapshot.
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil {
		return errors.New("managed service context is unavailable")
	}
	if err := snapshot.RequireService("user-service"); err != nil {
		return errors.New("managed service identity is invalid")
	}
	binding, err := snapshot.Binding(permissionBindingName)
	if err != nil || binding.APIID != sharedperm.DefaultPermissionCheckApiID {
		return errors.New("required permission API binding is unavailable")
	}
	if _, err := snapshot.Client(); err != nil {
		return errors.New("required permission API client is unavailable")
	}
	if _, err := s.Context.Credential(ctx); err != nil {
		return errors.New("workload credential is unavailable")
	}
	return nil
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.Context != nil {
		_ = s.Context.Close()
	}
	if s.DB != nil {
		s.DB.Close()
	}
	_ = ctx
}
