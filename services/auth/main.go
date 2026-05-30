package main

import (
	"context"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"

	"ojos-auth/internal/app"
	"ojos-auth/internal/router"

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

	a.Logger.Info("auth listening", zap.Int("port", a.Cfg.Service.Port))

	go server.Start()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)

	<-quit

	a.Logger.Info("auth shutting down")

	server.Stop()

	if err := a.Close(context.Background()); err != nil {
		a.Logger.Error("auth close failed", zap.Error(err))
	}

	a.Logger.Info("auth stopped")
}
