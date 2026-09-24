// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"
	"sync"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/repository"
	"ojos-shared/eventing"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider
	Redis  *redis.Client
	// Events is the immutable Release Event Contract materialized by the Agent.
	// EventRedis is deliberately separate from the service's legacy Redis client:
	// managed event traffic must use the Agent-local connection selected by the
	// Orchestrator, while unmanaged development may keep using REDIS_URL.
	Events     *eventing.EventContext
	EventRedis redis.UniversalClient

	Repo       *repository.Repository
	Permission sharedperm.UserChecker
	Context    *servicecontext.ContextProvider
	ArtifactGC *ArtifactGCController
	Managed    bool

	InternalAuthMiddleware rest.Middleware
	UserContextMiddleware  rest.Middleware

	backgroundCancel context.CancelFunc
	backgroundWG     sync.WaitGroup
}

const (
	permissionBindingName     = sharedperm.DefaultPermissionCheckApiID
	storagePutBinding         = "storage.object.put"
	storageHeadBinding        = "storage.object.head"
	storageDeleteBinding      = "storage.object.delete"
	defaultProblemsOutputFile = "/run/ojos/resources/problems/dsn"
	managedProblemsRoot       = "/data/ojos/problems"
)

func (s *ServiceContext) ActivePermissionChecker() sharedperm.UserChecker {
	if s == nil {
		return nil
	}
	if s.Permission != nil {
		return s.Permission
	}
	return sharedperm.NewDatabaseUserChecker(s.DB)
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.backgroundCancel != nil {
		s.backgroundCancel()
	}
	done := make(chan struct{})
	go func() {
		s.backgroundWG.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-ctx.Done():
	}

	if s.EventRedis != nil && s.EventRedis != s.Redis {
		_ = s.EventRedis.Close()
	}
	if s.Context != nil {
		_ = s.Context.Close()
	}

	if s.Redis != nil {
		_ = s.Redis.Close()
	}

	if s.DB != nil {
		s.DB.Close()
	}

	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}

	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
