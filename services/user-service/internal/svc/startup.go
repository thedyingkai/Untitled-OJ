// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-shared/database"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
	"ojos-user-service/internal/config"
	"ojos-user-service/internal/middleware"
	"ojos-user-service/internal/store"

	"github.com/jackc/pgx/v5/pgxpool"
)

func NewServiceContext(c config.Config) (result *ServiceContext, startupErr error) {
	serviceContext := &ServiceContext{}
	defer func() {
		if result == nil {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			serviceContext.Close(shutdownCtx)
		}
	}()
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
		serviceContext.DB = db
	}
	if managedEnvironment() && db == nil {
		return nil, errors.New("managed user-service requires its claimed PostgreSQL database")
	}

	profileStore, err := store.NewProfileStore(c.Storage.ProfilesRoot, db)
	if err != nil {
		return nil, fmt.Errorf("configure user profile store: %w", err)
	}

	var contextProvider *servicecontext.ContextProvider
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, fmt.Errorf("load managed service context: %w", err)
	}
	var permissionChecker sharedperm.UserChecker
	if contextValue != nil {
		if err := contextValue.RequireService("user-service"); err != nil {
			return nil, err
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		serviceContext.Context = contextProvider
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			return nil, fmt.Errorf("configure permission ApiBinding: %w", err)
		}
	} else {
		if managedEnvironment() {
			return nil, errors.New("managed user-service requires an Agent service context")
		}
		permissionChecker = sharedperm.NewUserCheckerWithConfig(permissionCheckerConfig(c), db)
		if permissionChecker == nil {
			return nil, errors.New("permission checker is not configured")
		}
	}
	*serviceContext = ServiceContext{
		Config:                c,
		ProfileStore:          profileStore,
		DB:                    db,
		Permission:            permissionChecker,
		Context:               contextProvider,
		Managed:               managedEnvironment(),
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
	return serviceContext, nil
}
