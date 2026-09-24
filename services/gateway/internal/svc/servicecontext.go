// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"
	"net/http"
	"sync"
	"time"

	"ojos-gateway/internal/authclient"
	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	gtopology "ojos-gateway/internal/topologyprojection"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"

	"github.com/redis/go-redis/v9"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type ServiceContext struct {
	Config config.Config

	Logger *zap.Logger
	Redis  *redis.Client
	Tracer *sdktrace.TracerProvider

	Proxy               http.HandlerFunc
	ServiceProxy        *proxy.ServiceProxy
	ServiceStatusDriver servicestatus.ServiceStatusDriver
	RouteTableOptions   servicestatus.RouteTableOptions
	InternalSigner      *internalauth.Signer
	Orchestrator        *orchestratorsnapshot.Client
	AuthClient          *authclient.Client
	PermissionChecker   sharedperm.UserChecker
	Context             *servicecontext.ContextProvider
	TopologyProjection  *gtopology.Store

	contributionCancel  context.CancelFunc
	contributionDone    chan struct{}
	contributionMu      sync.Mutex
	contributionDigest  string
	contributionAcked   string
	contributionPending *orchestratorsnapshot.ContributionSnapshot
	contributionReady   bool
	contributionError   string
}

const contributionSnapshotPollInterval = 5 * time.Second

const permissionBindingName = sharedperm.DefaultPermissionCheckApiID

func (s *ServiceContext) Close(ctx context.Context) {
	if s == nil {
		return
	}
	s.contributionMu.Lock()
	cancel := s.contributionCancel
	done := s.contributionDone
	s.contributionCancel = nil
	s.contributionDone = nil
	s.contributionMu.Unlock()
	if cancel != nil {
		cancel()
	}
	if done != nil {
		select {
		case <-done:
		case <-ctx.Done():
		}
	}
	if s.ServiceProxy != nil {
		s.ServiceProxy.Close()
	}
	if s.Context != nil {
		_ = s.Context.Close()
	}

	if s.Redis != nil {
		_ = s.Redis.Close()
	}

	if s.Tracer != nil {
		_ = s.Tracer.Shutdown(ctx)
	}

	if s.Logger != nil {
		_ = s.Logger.Sync()
	}
}
