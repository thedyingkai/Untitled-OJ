// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/contributionprojection"
	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"
	atopology "ojos-auth-service/internal/topologyprojection"
	"ojos-shared/security/workload"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/zeromicro/go-zero/rest"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	DB     *pgxpool.Pool
	Tracer *sdktrace.TracerProvider

	UserRepo       *repository.UserRepository
	AdminRepo      *repository.AdminRepository
	AuthService    *service.AuthService
	AdminBootstrap *service.AdminBootstrapService

	AuthMiddleware                 rest.Middleware
	PermissionReadMiddleware       rest.Middleware
	DelegatedPermissionMiddleware  rest.Middleware
	WorkloadControlPlaneMiddleware rest.Middleware
	WorkloadIssuer                 *workload.Issuer
	TopologyProjection             *atopology.Store
	ContributionProjection         *contributionprojection.Reconciler
}

func (s *ServiceContext) Close(ctx context.Context) {
	if s.ContributionProjection != nil {
		s.ContributionProjection.Close()
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
