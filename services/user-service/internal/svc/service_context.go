// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"

	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
	"ojos-user-service/internal/config"
	"ojos-user-service/internal/store"

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

func (s *ServiceContext) ActivePermissionChecker() sharedperm.UserChecker {
	if s == nil {
		return nil
	}
	if s.Permission != nil {
		return s.Permission
	}
	return sharedperm.NewDatabaseUserChecker(s.DB)
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
