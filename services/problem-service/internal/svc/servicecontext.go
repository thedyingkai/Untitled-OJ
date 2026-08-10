// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package svc

import (
	"context"
	"errors"
	"fmt"
	"log"
	"os"
	"strings"
	"sync"
	"time"

	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/middleware"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/projection"
	"ojos-problem-service/internal/repository"
	"ojos-shared/eventing"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/tracing"

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
	ArtifactGC *ArtifactGCController

	InternalAuthMiddleware rest.Middleware
	UserContextMiddleware  rest.Middleware

	backgroundCancel context.CancelFunc
	backgroundWG     sync.WaitGroup
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		log.Fatalf("init logger failed: %v", err)
	}

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		log.Fatalf("init tracing failed: %v", err)
	}

	db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
	if err != nil {
		log.Fatalf("connect postgres failed: %v", err)
	}

	eventContext, err := eventing.LoadEventContextForService(
		"problem-service",
		[]string{eventing.ProblemDeletedV1, eventing.ProblemSnapshotV1},
		nil,
	)
	if err != nil {
		log.Fatalf("configure managed Event Contract failed: %v", err)
	}
	var redisClient *redis.Client
	if eventContext != nil {
		managedRedis, managedErr := eventContext.RedisClient()
		if managedErr != nil {
			log.Fatalf("load Agent-local event connection failed: %v", managedErr)
		}
		if managedErr = managedRedis.Ping(ctx).Err(); managedErr != nil {
			_ = managedRedis.Close()
			log.Fatalf("ping Agent-local event connection failed: %v", managedErr)
		}
		// In a managed deployment the Agent-local connection is the only
		// Redis credential delivered to the process. Reuse it for nonce state
		// and event relay instead of requiring an undeclared REDIS_URL.
		redisClient = managedRedis
	} else {
		redisOptions, parseErr := redis.ParseURL(c.Redis.Url)
		if parseErr != nil {
			log.Fatalf("parse redis url failed: %v", parseErr)
		}
		redisClient = redis.NewClient(redisOptions)
		if pingErr := redisClient.Ping(ctx).Err(); pingErr != nil {
			log.Fatalf("ping redis failed: %v", pingErr)
		}
	}
	var eventRedis redis.UniversalClient = redisClient
	permissionChecker, err := sharedperm.NewManagedOrLegacyUserChecker(
		"problem-service",
		sharedperm.DefaultPermissionCheckBinding,
		permissionCheckerConfig(c),
		db,
	)
	if err != nil {
		log.Fatalf("configure permission_check ApiBinding failed: %v", err)
	}

	internalAuthCfg := internalauth.Config{
		Enabled:       c.InternalAuth.Enabled,
		TimestampSkew: time.Duration(c.InternalAuth.TimestampSkewSeconds) * time.Second,
		NonceTTL:      time.Duration(c.InternalAuth.NonceTTLSeconds) * time.Second,
	}

	var internalVerifier *internalauth.Verifier
	if c.InternalAuth.Enabled {
		internalKeyManager := internalauth.NewKeyManager(db, internalAuthCfg)
		internalNonceStore := internalauth.RedisNonceStore{
			Client: redisClient,
			Prefix: "ojos:internal-auth:nonce:",
		}

		internalVerifier = internalauth.NewVerifier(
			internalKeyManager,
			internalNonceStore,
			internalAuthCfg,
		)
	}

	svcCtx := &ServiceContext{
		Config: c,

		Logger:     zlog,
		DB:         db,
		Tracer:     tp,
		Redis:      redisClient,
		Events:     eventContext,
		EventRedis: eventRedis,

		Repo:       repository.New(db),
		Permission: permissionChecker,

		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
	if err := packagemutation.RecoverAll(ctx, svcCtx.Repo, c.Storage.ProblemsRoot); err != nil {
		log.Fatalf("recover problem package mutation journal failed: %v", err)
	}
	svcCtx.startProjectionBackground()
	return svcCtx
}

func (s *ServiceContext) startProjectionBackground() {
	ctx, cancel := context.WithCancel(context.Background())
	s.backgroundCancel = cancel
	stream := ""
	if s.Events != nil {
		stream = s.Events.Stream
	} else {
		// Compatibility is intentionally limited to unmanaged development.
		stream = strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_STREAM"))
		if stream == "" {
			stream = "ojos:integration:problem:v1"
		}
	}
	relay := &eventing.Relay{
		DB:            s.DB,
		Redis:         s.EventRedis,
		Stream:        stream,
		RelayID:       s.Config.Name,
		BatchSize:     100,
		LeaseDuration: 30 * time.Second,
		PollInterval:  250 * time.Millisecond,
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_REPLAY_ON_START")); value == "1" || strings.EqualFold(value, "true") {
		if replayed, err := relay.ReplayPublished(ctx); err != nil {
			s.Logger.Warn("problem integration-event replay preparation failed", zap.Error(err))
		} else {
			s.Logger.Info("problem integration events scheduled for replay", zap.Int64("events", replayed))
		}
	}
	s.backgroundWG.Add(2)
	go func() {
		defer s.backgroundWG.Done()
		relay.Run(ctx)
	}()
	go func() {
		defer s.backgroundWG.Done()
		for {
			if _, err := projection.BackfillOnce(ctx, s.Repo, s.Config.Storage); err != nil && ctx.Err() == nil {
				s.Logger.Warn("problem projection backfill failed", zap.Error(err))
			}
			timer := time.NewTimer(5 * time.Minute)
			select {
			case <-ctx.Done():
				timer.Stop()
				return
			case <-timer.C:
			}
		}
	}()
	s.startArtifactGC(ctx)
}

func (s *ServiceContext) startArtifactGC(ctx context.Context) {
	production := strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") || envBool("OJOS_MANAGED_WORKLOAD")
	if !envBoolDefault("OJOS_PROBLEM_ARTIFACT_GC_ENABLED", production) {
		return
	}
	retention, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_RETENTION", artifactgc.DefaultRetention)
	if err != nil || retention < artifactgc.MinimumRetention {
		if production {
			log.Fatalf("configure production problem artifact GC retention failed: %v", err)
		}
		s.Logger.Error("problem artifact GC disabled: invalid retention", zap.Duration("retention", retention), zap.Error(err))
		return
	}
	interval, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_INTERVAL", 24*time.Hour)
	if err != nil || interval < 5*time.Minute {
		if production {
			log.Fatalf("configure production problem artifact GC interval failed: %v", err)
		}
		s.Logger.Error("problem artifact GC disabled: invalid interval", zap.Duration("interval", interval), zap.Error(err))
		return
	}
	store, err := artifactgc.NewBoundObjectStore(bucketName(s.Config.Storage.Bucket))
	if err != nil {
		if production {
			log.Fatalf("configure production problem artifact GC ApiBindings failed: %v", err)
		}
		s.Logger.Error("problem artifact GC disabled: configure bound Storage", zap.Error(err))
		return
	}
	timing, err := configuredArtifactGCDeleteTiming(store)
	if err != nil {
		if production {
			log.Fatalf("configure production problem artifact GC delete isolation failed: %v", err)
		}
		s.Logger.Error("problem artifact GC disabled: unsafe delete isolation timing", zap.Error(err))
		return
	}
	collector := artifactgc.Collector{
		Ledger:        artifactgc.PostgresLedger{DB: s.DB},
		Store:         store,
		Retention:     retention,
		ClaimLease:    timing.ClaimLease,
		DeleteTimeout: timing.DeleteTimeout,
		Delete:        envBoolDefault("OJOS_PROBLEM_ARTIFACT_GC_DELETE", production),
		BatchSize:     100,
	}
	controller := NewArtifactGCController(artifactgc.PostgresLedger{DB: s.DB}, collector)
	s.ArtifactGC = controller
	s.backgroundWG.Add(1)
	go func() {
		defer s.backgroundWG.Done()
		controller.RunLoop(ctx, interval, func(report artifactgc.Report, runErr error) {
			fields := []zap.Field{
				zap.Bool("dry_run", report.DryRun),
				zap.Int("scanned", report.Scanned),
				zap.Int("referenced", report.Referenced),
				zap.Int("candidates", len(report.Candidates)),
				zap.Int("deleted", len(report.Deleted)),
				zap.Duration("claim_lease", timing.ClaimLease),
				zap.Duration("delete_timeout", timing.DeleteTimeout),
				zap.Duration("delete_isolation_grace", timing.Grace),
			}
			if runErr != nil && ctx.Err() == nil {
				s.Logger.Warn("problem artifact GC scan failed closed", append(fields, zap.Error(runErr))...)
			} else if ctx.Err() == nil {
				s.Logger.Info("problem artifact GC scan completed", fields...)
			}
		})
	}()
}

func envBool(key string) bool {
	value := strings.TrimSpace(os.Getenv(key))
	return value == "1" || strings.EqualFold(value, "true")
}

func envBoolDefault(key string, fallback bool) bool {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback
	}
	return value == "1" || strings.EqualFold(value, "true")
}

func envDuration(key string, fallback time.Duration) (time.Duration, error) {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback, nil
	}
	return time.ParseDuration(value)
}

func configuredArtifactGCDeleteTiming(store *artifactgc.BoundObjectStore) (artifactgc.DeleteIsolationTiming, error) {
	if store == nil {
		return artifactgc.DeleteIsolationTiming{}, errors.New("artifact GC bound object store is required")
	}
	claimLease, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE", artifactgc.DefaultClaimLease)
	if err != nil {
		return artifactgc.DeleteIsolationTiming{}, fmt.Errorf("parse artifact GC claim lease: %w", err)
	}
	deleteTimeout, err := store.DeleteBindingTimeout()
	if err != nil {
		return artifactgc.DeleteIsolationTiming{}, err
	}
	return artifactgc.ResolveDeleteIsolationTiming(claimLease, deleteTimeout)
}

func bucketName(value string) string {
	if value = strings.TrimSpace(value); value != "" {
		return value
	}
	return "problems"
}

// permissionCheckerConfig keeps the routing decision in one place: gateway +
// api_id first, direct auth-service address only as a fallback.
func permissionCheckerConfig(c config.Config) sharedperm.RemoteCheckerConfig {
	return sharedperm.RemoteCheckerConfig{
		InternalGatewayEndpoint: c.AuthService.InternalGatewayEndpoint,
		ApiID:                   c.AuthService.PermissionCheckApiID,
		CallerService:           c.AuthService.CallerService,
		CallerNodeID:            c.AuthService.CallerNodeID,
		ServiceToken:            c.AuthService.ServiceToken,
		AuthServiceEndpoint:     c.AuthService.Endpoint,
		AuthServiceAdminToken:   c.AuthService.AdminToken,
	}
}

func applyEnvOverrides(c *config.Config) {
	if value := firstEnv("PROBLEM_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
		c.Redis.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
	}
	if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
		c.AuthService.AdminToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEMS_ROOT")); value != "" {
		c.Storage.ProblemsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_URL")); value != "" {
		c.Storage.ServiceEndpoint = value
	}
	if value := firstEnv("OJOS_INTERNAL_GATEWAY_ENDPOINT", "OJOS_INTERNAL_GATEWAY_URL"); value != "" {
		c.Storage.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_HEAD_API_ID")); value != "" {
		c.Storage.HeadApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_STORAGE_BUCKET")); value != "" {
		c.Storage.Bucket = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.Storage.ServiceToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.Storage.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.Storage.CallerNodeID = value
	}
	// Deliberately a dedicated variable rather than reusing
	// OJOS_INTERNAL_GATEWAY_ENDPOINT / OJOS_INTERNAL_GATEWAY_URL (which already
	// drive the storage client): switching the permission check onto the gateway
	// also requires a service credential and a service permission grant, so it
	// must be an explicit opt-in per deployment.
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT")); value != "" {
		c.AuthService.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CHECK_API_ID")); value != "" {
		c.AuthService.PermissionCheckApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.AuthService.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.AuthService.CallerNodeID = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.AuthService.ServiceToken = value
	}
}

func (s *ServiceContext) ActivePermissionChecker() sharedperm.UserChecker {
	if s == nil {
		return nil
	}
	if s.Permission != nil {
		return s.Permission
	}
	return sharedperm.NewDatabaseUserChecker(s.DB)
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
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
