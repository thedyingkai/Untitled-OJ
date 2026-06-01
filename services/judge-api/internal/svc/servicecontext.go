package svc

import (
	"context"
	"log"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
)

type ServiceContext struct {
	Config config.Config

	DB    *pgxpool.Pool
	Repo  *repository.Repository
	Redis *redis.Client

	UserContextMiddleware rest.Middleware
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()

	db, err := pgxpool.New(ctx, c.Database.Url)
	if err != nil {
		log.Fatalf("connect postgres failed: %v", err)
	}

	if err := db.Ping(ctx); err != nil {
		log.Fatalf("ping postgres failed: %v", err)
	}

	redisOptions, err := redis.ParseURL(c.Redis.Url)
	if err != nil {
		log.Fatalf("parse redis url failed: %v", err)
	}

	redisClient := redis.NewClient(redisOptions)

	if err := redisClient.Ping(ctx).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
	}

	repo := repository.New(db)

	return &ServiceContext{
		Config: c,
		DB:     db,
		Repo:   repo,
		Redis:  redisClient,

		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
}
