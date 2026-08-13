// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"

	sharedmw "ojos-shared/middleware"
	"ojos-shared/servicehealth"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/handler"
	"ojos-storage-service/internal/svc"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/storageservice.yaml", "the config file")

func main() {
	if handled, err := servicehealth.RunIfRequested(os.Args, "http://127.0.0.1:8085/readyz"); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
	if err := config.ApplyEnvironment(&c); err != nil {
		fmt.Fprintln(os.Stderr, "invalid storage runtime configuration:", err)
		os.Exit(1)
	}
	c.PrepareObjectStreaming()
	sharedmw.InstallHTTPErrorHandler()

	server := rest.MustNewServer(c.RestConf)
	defer server.Stop()

	ctx := svc.NewServiceContext(c)
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		ctx.Close(shutdownCtx)
	}()
	server.Use(sharedmw.RecoveryMiddleware(ctx.Logger))
	server.Use(sharedmw.ServiceLoggingMiddleware("storage-service", ctx.Logger, ctx.Tracer))

	handler.RegisterHandlers(server, ctx)
	sharedmw.RegisterMetricsRoute(server)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}
