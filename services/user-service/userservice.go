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

	sharedmw "ojos-shared/middleware"
	"ojos-user-service/internal/config"
	"ojos-user-service/internal/handler"
	"ojos-user-service/internal/svc"

	"github.com/zeromicro/go-zero/core/conf"
	"github.com/zeromicro/go-zero/rest"
)

var configFile = flag.String("f", "etc/userservice.yaml", "the config file")

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

	ctx, err := svc.NewServiceContext(c)
	if err != nil {
		log.Fatalf("start user-service: %v", err)
	}
	defer ctx.Close(context.Background())

	server := rest.MustNewServer(c.RestConf)
	defer server.Stop()

	server.Use(sharedmw.ServiceLoggingMiddleware("user-service", nil, nil))
	handler.RegisterHandlers(server, ctx)
	sharedmw.RegisterMetricsRoute(server)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}

func readycheck() error {
	client := &http.Client{
		Timeout: 2 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	request, err := http.NewRequest(http.MethodGet, "http://127.0.0.1:8084/readyz", nil)
	if err != nil {
		return err
	}
	response, err := client.Do(request)
	if err != nil {
		return fmt.Errorf("user-service readiness probe failed: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("user-service readiness probe returned %s", response.Status)
	}
	return nil
}
