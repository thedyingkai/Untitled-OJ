// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strings"
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
	if handled, err := servicehealth.RunIfRequested(os.Args, "http://127.0.0.1:8085/health"); handled {
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	flag.Parse()

	var c config.Config
	conf.MustLoad(*configFile, &c)
	applyEnvOverrides(&c)
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
	server.Use(sharedmw.LoggingMiddleware(ctx.Logger, ctx.Tracer))

	handler.RegisterHandlers(server, ctx)

	fmt.Printf("Starting server at %s:%d...\n", c.Host, c.Port)
	server.Start()
}

func applyEnvOverrides(c *config.Config) {
	if value := strings.TrimSpace(os.Getenv("STORAGE_BACKEND")); value != "" {
		c.Storage.Backend = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_BACKEND")); value != "" {
		c.Storage.Backend = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_ROOT")); value != "" {
		c.Storage.Root = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_BUCKETS")); value != "" {
		c.Storage.Buckets = splitCSV(value)
	}
	if value := strings.TrimSpace(os.Getenv("MINIO_ENDPOINT")); value != "" {
		c.Storage.MinIO.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("MINIO_ACCESS_KEY")); value != "" {
		c.Storage.MinIO.AccessKey = value
	}
	if value := strings.TrimSpace(os.Getenv("MINIO_SECRET_KEY")); value != "" {
		c.Storage.MinIO.SecretKey = value
	}
	if value := strings.TrimSpace(os.Getenv("MINIO_USE_SSL")); value != "" {
		c.Storage.MinIO.UseSSL = parseBool(value)
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
}

func splitCSV(value string) []string {
	parts := strings.Split(value, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if part = strings.TrimSpace(part); part != "" {
			out = append(out, part)
		}
	}
	return out
}

func parseBool(value string) bool {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "1", "true", "yes", "on":
		return true
	default:
		return false
	}
}
