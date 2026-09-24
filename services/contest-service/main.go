package main

import (
	"context"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"ojos-contest-service/internal/app"
	"ojos-contest-service/internal/config"
)

func main() {
	if len(os.Args) == 2 && (os.Args[1] == "healthcheck" || os.Args[1] == "readycheck") {
		client := &http.Client{Timeout: 2 * time.Second}
		path := "/healthz"
		if os.Args[1] == "readycheck" {
			path = "/readyz"
		}
		response, err := client.Get("http://127.0.0.1:8080" + path)
		if err != nil || response.StatusCode != http.StatusOK {
			os.Exit(1)
		}
		_ = response.Body.Close()
		return
	}
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if err := run(logger); err != nil {
		logger.Error("contest service stopped", "error", err)
		os.Exit(1)
	}
}

func run(logger *slog.Logger) error {
	runtimeConfig, err := config.Load()
	if err != nil {
		return err
	}
	rootContext, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	return app.Run(rootContext, runtimeConfig, logger)
}
