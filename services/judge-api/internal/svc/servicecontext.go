package svc

import (
	"context"
	"log"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider

	Repo  *repository.Repository
	Redis *redis.Client

	UserContextMiddleware rest.Middleware
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}

	db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
	if err != nil {
		log.Fatalf("connect postgres failed: %v", err)
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

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		Repo:  repo,
		Redis: redisClient,

		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.Redis != nil {
		_ = s.Redis.Close()
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
