// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"

	"ojos-problem-api/internal/config"
	"ojos-problem-api/internal/handler"
	"ojos-problem-api/internal/svc"

	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/problemapi.yaml", "the config file")

func main() {
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
	sharedmw.InstallHTTPErrorHandler()

	svcCtx := svc.NewServiceContext(c)
	defer svcCtx.Close(context.Background())

	server := rest.MustNewServer(c.RestConf)
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer))

	handler.RegisterHandlers(server, svcCtx)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}
