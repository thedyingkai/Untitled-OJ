// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"

	"ojos-shared/security/workload"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/store"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config              config.Config
	ObjectStore         store.ObjectStorage
	Logger              *zap.Logger
	Tracer              *sdktrace.TracerProvider
	WorkloadVerifier    *workload.Verifier
	WorkloadAuthEnabled bool
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
