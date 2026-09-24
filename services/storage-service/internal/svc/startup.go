// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"errors"
	"os"
	"strings"
	"time"

	sharedlogger "ojos-shared/logger"
	"ojos-shared/tracing"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"
)

func NewServiceContext(c config.Config) *ServiceContext {
	result, err := BuildServiceContext(c)
	if err != nil {
		panic(err)
	}
	return result
}

func BuildServiceContext(c config.Config) (result *ServiceContext, startupErr error) {
	serviceContext := &ServiceContext{}
	defer func() {
		if result == nil {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			serviceContext.Close(shutdownCtx)
		}
	}()
	ctx := context.Background()
	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, err
	}
	serviceContext.Logger = zlog
	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, err
	}
	serviceContext.Tracer = tp
	objectStore, err := store.NewObjectStorage(store.Options{
		Backend: c.Storage.Backend,
		Root:    c.Storage.Root,
		Buckets: c.Storage.Buckets,
		S3: store.S3Options{
			Endpoint:  c.Storage.S3.Endpoint,
			AccessKey: c.Storage.S3.AccessKey,
			SecretKey: c.Storage.S3.SecretKey,
			UseSSL:    c.Storage.S3.UseSSL,
			Region:    c.Storage.S3.Region,
		},
		MinIO: store.MinIOOptions{
			Endpoint:  c.Storage.MinIO.Endpoint,
			AccessKey: c.Storage.MinIO.AccessKey,
			SecretKey: c.Storage.MinIO.SecretKey,
			UseSSL:    c.Storage.MinIO.UseSSL,
		},
	})
	if err != nil {
		return nil, err
	}
	workloadVerifier, err := workloadVerifier(c.WorkloadIdentity)
	if err != nil {
		return nil, err
	}
	workloadAuthEnabled := config.ProductionEnvironment()
	if workloadAuthEnabled && workloadVerifier == nil {
		return nil, errors.New("production storage requires a workload identity verifier")
	}
	if config.ManagedEnvironment() && strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE")) == "" {
		return nil, errors.New("managed storage requires an Agent-materialized service context path")
	}
	*serviceContext = ServiceContext{
		Config:              c,
		ObjectStore:         objectStore,
		Logger:              zlog,
		Tracer:              tp,
		WorkloadVerifier:    workloadVerifier,
		WorkloadAuthEnabled: workloadAuthEnabled,
	}
	return serviceContext, nil
}
