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
	return &ServiceContext{
		Config:       c,
		ProfileStore: profileStore,
		DB:           db,
		Permission: sharedperm.NewUserChecker(
			c.AuthService.Endpoint,
			c.AuthService.AdminToken,
			db,
		),
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
