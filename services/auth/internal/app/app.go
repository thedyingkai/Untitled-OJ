package app

import (
	"context"
	"net/http"

	"ojos-shared/configs"
	"ojos-shared/database"
	"ojos-shared/events"
	"ojos-shared/logger"
	sharedmw "ojos-shared/middleware"
	"ojos-shared/tracing"

	"github.com/jackc/pgx/v5/pgxpool"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type App struct {
	Cfg      *configs.Config
	Logger   *zap.Logger
	DB       *pgxpool.Pool
	Tracer   *sdktrace.TracerProvider
	EventBus *events.Bus
}

func New(ctx context.Context) (*App, error) {
	cfg, err := configs.Load()
	if err != nil {
		return nil, err
	}

	logg, err := logger.New(cfg.Service.Name)
	if err != nil {
		return nil, err
	}

	tp, err := tracing.Init(ctx, cfg)
	if err != nil {
		return nil, err
	}

	pool, err := database.NewPostgresPool(ctx, cfg)
	if err != nil {
		return nil, err
	}

	bus, err := events.NewBus(cfg)
	if err != nil {
		return nil, err
	}

	tracer := tp.Tracer("auth")
	_, span := tracer.Start(ctx, "auth.startup")
	span.End()
	_ = tp.ForceFlush(ctx)

	return &App{
		Cfg:      cfg,
		Logger:   logg,
		DB:       pool,
		Tracer:   tp,
		EventBus: bus,
	}, nil
}

func (a *App) LoggingMiddleware(next http.HandlerFunc) http.HandlerFunc {
	return sharedmw.Logging(a.Logger, a.Tracer, next)
}

func (a *App) Close(ctx context.Context) error {
	if a.DB != nil {
		a.DB.Close()
	}

	if a.EventBus != nil {
		a.EventBus.Close()
	}

	if a.Tracer != nil {
		if err := a.Tracer.Shutdown(ctx); err != nil {
			return err
		}
	}

	if a.Logger != nil {
		_ = a.Logger.Sync()
	}

	return nil
}
