// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"net/http"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/handler"
	"ojos-gateway/internal/svc"

	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/gateway.yaml", "the config file")

func main() {
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)

	svcCtx := svc.NewServiceContext(c)
	defer svcCtx.Close(context.Background())

	notFoundHandler := svcCtx.Proxy
	notFoundHandler = sharedmw.RecoveryMiddleware(svcCtx.Logger)(notFoundHandler)
	notFoundHandler = sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer)(notFoundHandler)

	server := rest.MustNewServer(
		c.RestConf,
		rest.WithNotFoundHandler(http.HandlerFunc(notFoundHandler)),
	)
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer))

	handler.RegisterHandlers(server, svcCtx)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}
