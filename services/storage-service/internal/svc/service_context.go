// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"log"

	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config      config.Config
	ObjectStore store.ObjectStorage
	Logger      *zap.Logger
	Tracer      *sdktrace.TracerProvider
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}
	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}
	objectStore, err := store.NewObjectStorage(store.Options{
		Backend: c.Storage.Backend,
		Root:    c.Storage.Root,
		Buckets: c.Storage.Buckets,
		MinIO: store.MinIOOptions{
			Endpoint:  c.Storage.MinIO.Endpoint,
			AccessKey: c.Storage.MinIO.AccessKey,
			SecretKey: c.Storage.MinIO.SecretKey,
			UseSSL:    c.Storage.MinIO.UseSSL,
		},
	})
	if err != nil {
		panic(err)
	}
	return &ServiceContext{
		Config:      c,
		ObjectStore: objectStore,
		Logger:      zlog,
		Tracer:      tp,
	}
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s == nil {
		return
	}
	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}
	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
