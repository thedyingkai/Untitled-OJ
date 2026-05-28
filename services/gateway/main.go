package main

import (
	"context"
	"log"
	"net/http"
	"ojos-shared/logger"
	sharedmw "ojos-shared/middleware"
	"strconv"

	"ojos-gateway/internal/db"
	"ojos-gateway/internal/tracing"

	"ojos-shared/config"

	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	"go.opentelemetry.io/otel"
	"go.uber.org/zap"
)

func main() {
	ctx := context.Background()

	cfg, err := config.Load()
	if err != nil {
		log.Fatal(err)
	}

	logg, err := logger.New(cfg.Service.Name)
	if err != nil {
		log.Fatal(err)
	}
	defer logg.Sync()

	tp, err := tracing.Init(ctx, cfg)
	if err != nil {
		log.Fatal(err)
	}
	defer tp.Shutdown(ctx)

	pool, err := db.Connect(ctx, cfg)
	if err != nil {
		log.Fatal(err)
	}
	defer pool.Close()

	tracer := otel.Tracer("gateway")
	_, span := tracer.Start(ctx, "gateway.startup")
	span.End()
	_ = tp.ForceFlush(ctx)

	mux := http.NewServeMux()

	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})

	tracedHandler := otelhttp.NewHandler(
		sharedmw.Logging(logg, mux),
		"gateway-http",
	)

	addr := ":" + strconv.Itoa(cfg.Service.Port)
	logg.Info("gateway listening", zap.String("addr", addr))

	if err := http.ListenAndServe(addr, tracedHandler); err != nil {
		log.Fatal(err)
	}

}
