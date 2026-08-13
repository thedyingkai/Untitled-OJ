// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/handler"
	"ojos-auth-service/internal/svc"

	sharedmw "ojos-shared/middleware"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/auth.yaml", "the config file")

func main() {
	if len(os.Args) > 1 && os.Args[1] == "readycheck" {
		if err := readycheck(); err != nil {
			log.Print(err)
			os.Exit(1)
		}
		return
	}
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
	sharedmw.InstallHTTPErrorHandler()

	svcCtx, err := svc.NewServiceContext(c)
	if err != nil {
		log.Fatalf("start auth-service: %v", err)
	}
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		svcCtx.Close(ctx)
	}()

	server := rest.MustNewServer(c.RestConf)
	defer server.Stop()

	server.Use(sharedmw.RecoveryMiddleware(svcCtx.Logger))
	server.Use(sharedmw.ServiceLoggingMiddleware("auth-service", svcCtx.Logger, svcCtx.Tracer))

	handler.RegisterHandlers(server, svcCtx)
	sharedmw.RegisterMetricsRoute(server)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}

func readycheck() error {
	client := &http.Client{
		Timeout:       2 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse },
	}
	request, err := http.NewRequest(http.MethodGet, "http://127.0.0.1:8081/readyz", nil)
	if err != nil {
		return err
	}
	response, err := client.Do(request)
	if err != nil {
		return fmt.Errorf("auth-service readiness probe failed: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("auth-service readiness probe returned %s", response.Status)
	}
	return nil
}
