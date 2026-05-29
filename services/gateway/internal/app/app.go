package app

import (
	"context"
	"net/http"
	"ojos-shared/database"
	"ojos-shared/events"
	sharedmw "ojos-shared/middleware"
	"ojos-shared/tracing"

	"ojos-shared/config"
	"ojos-shared/logger"

	"github.com/jackc/pgx/v5/pgxpool"
	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type App struct {
	Cfg      *config.Config
	Logger   *zap.Logger
	DB       *pgxpool.Pool
	Tracer   *sdktrace.TracerProvider
	EventBus *events.Bus
}

func New(ctx context.Context) (*App, error) {
	cfg, err := config.Load()
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

	tracer := otel.Tracer("gateway")
	_, span := tracer.Start(ctx, "gateway.startup")
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

	if a.Tracer != nil {
		if err := a.Tracer.Shutdown(ctx); err != nil {
			return err
		}
	}

	if a.Logger != nil {
		_ = a.Logger.Sync()
	}

	if a.EventBus != nil {
		a.EventBus.Close()
	}

	return nil
}
