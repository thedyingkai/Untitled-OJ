// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/handler"
	gatewaymw "ojos-gateway/internal/middleware"
	"ojos-gateway/internal/proxy"
	"ojos-gateway/internal/svc"

	sharedmw "ojos-shared/middleware"
	"ojos-shared/servicehealth"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/gateway.yaml", "the config file")

func main() {
	if handled, err := servicehealth.RunIfRequested(os.Args, "http://127.0.0.1:8080/readyz"); handled {
		if err != nil {
			log.Fatal(err)
		}
		return
	}
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
	c.PrepareProxyServer()
	sharedmw.InstallHTTPErrorHandler()

	svcCtx := svc.NewServiceContext(c)
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		svcCtx.Close(ctx)
	}()

	server := rest.MustNewServer(c.RestConf)
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.ServiceLoggingMiddleware("gateway", svcCtx.Logger, svcCtx.Tracer))
	server.Use(gatewaymw.CORSMiddleware())

	handler.RegisterHandlers(server, svcCtx)
	sharedmw.RegisterMetricsRoute(server)

	proxy.RegisterRoutes(server, svcCtx.Config.Proxy.Routes, svcCtx.Proxy)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}
