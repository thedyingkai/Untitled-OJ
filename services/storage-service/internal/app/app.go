// Package app owns storage-service assembly and reverse-order shutdown.
// The GoZero transport retains its existing process-signal and HTTP drain behavior.
package app

import (
	"context"
	"fmt"
	"time"

	sharedmw "ojos-shared/middleware"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/handler"
	"ojos-storage-service/internal/svc"

	"github.com/zeromicro/go-zero/rest"
)

// Run returns configuration/assembly failures after releasing acquired resources.
func Run(c config.Config) error {
	if err := config.ApplyEnvironment(&c); err != nil {
		return fmt.Errorf("invalid storage runtime configuration: %w", err)
	}
	c.PrepareObjectStreaming()
	sharedmw.InstallHTTPErrorHandler()

	svcCtx, err := svc.BuildServiceContext(c)
	if err != nil {
		return err
	}
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		svcCtx.Close(shutdownCtx)
	}()
	server, err := rest.NewServer(c.RestConf)
	if err != nil {
		return fmt.Errorf("configure HTTP server: %w", err)
	}
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.ServiceLoggingMiddleware("storage-service", svcCtx.Logger, svcCtx.Tracer))

	handler.RegisterHandlers(server, svcCtx)
	sharedmw.RegisterMetricsRoute(server)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
	return nil
}
