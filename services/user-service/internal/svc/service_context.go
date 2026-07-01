// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"

	"ojos-user-service/internal/config"
	"ojos-user-service/internal/store"

	"ojos-shared/database"

	"github.com/jackc/pgx/v5/pgxpool"
)

type ServiceContext struct {
	Config       config.Config
	ProfileStore *store.ProfileStore
	DB           *pgxpool.Pool
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()

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
	}
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.DB != nil {
		s.DB.Close()
	}
	_ = ctx
}
