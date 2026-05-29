package main

import (
	"context"
	"log"
	"net/http"
	"ojos-gateway/internal/router"
	"os"
	"os/signal"
	"syscall"

	"ojos-gateway/internal/app"
	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/rest"
	"go.uber.org/zap"
)

func main() {
	ctx := context.Background()

	a, err := app.New(ctx)
	if err != nil {
		log.Fatal(err)
	}

	server := rest.MustNewServer(rest.RestConf{
		Host: "0.0.0.0",
		Port: a.Cfg.Service.Port,
	})

	server.Use(func(next http.HandlerFunc) http.HandlerFunc {
		return sharedmw.Recovery(a.Logger, next)
	})

	server.Use(a.LoggingMiddleware)

	router.Register(server, a)

	//server.AddRoute(rest.Route{
	//	Method: http.MethodGet,
	//	Path:   "/trace-test",
	//	Handler: func(w http.ResponseWriter, r *http.Request) {
	//		ctx, span := otel.Tracer("gateway").Start(r.Context(), "TRACE_TEST")
	//		span.End()
	//
	//		_ = a.Tracer.ForceFlush(ctx)
	//
	//		w.WriteHeader(http.StatusOK)
	//		_, _ = w.Write([]byte("trace-test-ok"))
	//	},
	//})

	a.Logger.Info("gateway listening", zap.Int("port", a.Cfg.Service.Port))

	go server.Start()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)

	<-quit

	a.Logger.Info("gateway shutting down")

	server.Stop()

	if err := a.Close(context.Background()); err != nil {
		a.Logger.Error("gateway close failed", zap.Error(err))
	}

	a.Logger.Info("gateway stopped")
}
