// Package app owns gateway assembly and reverse-order shutdown.
// The GoZero transport retains its existing process-signal and HTTP drain behavior.
package app

import (
	"context"
	"fmt"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/handler"
	gatewaymw "ojos-gateway/internal/middleware"
	"ojos-gateway/internal/proxy"
	"ojos-gateway/internal/svc"
	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/rest"
)

// Run returns configuration/assembly failures after releasing acquired resources.
func Run(c config.Config) error {
	c.PrepareProxyServer()
	sharedmw.InstallHTTPErrorHandler()

	svcCtx, err := svc.NewServiceContext(c)
	if err != nil {
		return err
	}
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		svcCtx.Close(ctx)
	}()

	server, err := rest.NewServer(c.RestConf)
	if err != nil {
		return fmt.Errorf("configure HTTP server: %w", err)
	}
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.ServiceLoggingMiddleware("gateway", svcCtx.Logger, svcCtx.Tracer))
	server.Use(gatewaymw.CORSMiddleware())

	handler.RegisterHandlers(server, svcCtx)
	sharedmw.RegisterMetricsRoute(server)

	proxy.RegisterRoutes(server, svcCtx.Config.Proxy.Routes, svcCtx.Proxy)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
	return nil
}
