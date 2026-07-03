// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/handler"
	"ojos-judge-api/internal/svc"

	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/judgeapi.yaml", "the config file")

func main() {
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
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
	server.Use(sharedmw.LoggingMiddleware(svcCtx.Logger, svcCtx.Tracer))

	handler.RegisterHandlers(server, svcCtx)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}
