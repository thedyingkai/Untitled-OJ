// Package app owns user-service assembly and reverse-order shutdown.
// The GoZero transport retains its existing process-signal and HTTP drain behavior.
package app

import (
	"context"
	"fmt"
	"time"

	sharedmw "ojos-shared/middleware"
	"ojos-user-service/internal/config"
	"ojos-user-service/internal/handler"
	"ojos-user-service/internal/svc"

	"github.com/zeromicro/go-zero/rest"
)

// Run returns configuration/assembly failures after releasing acquired resources.
func Run(c config.Config) error {

	svcCtx, err := svc.NewServiceContext(c)
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

	server.Use(sharedmw.ServiceLoggingMiddleware("user-service", nil, nil))
	handler.RegisterHandlers(server, svcCtx)
	sharedmw.RegisterMetricsRoute(server)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
	return nil
}
