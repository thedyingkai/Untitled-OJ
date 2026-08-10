package svc

import (
	"context"
	"fmt"
	"log"
	"os"
	"strings"
	"sync"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-shared/eventing"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/security/workload"

	"ojos-shared/database"
	sharedlogger "ojos-shared/logger"
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

	Repo           *repository.Repository
	SubmissionRepo SubmissionRepository
	WorkerRepo     WorkerTaskRepository
	Permission     PermissionChecker
	Redis          *redis.Client
	Events         *eventing.EventContext
	EventRedis     redis.UniversalClient
	ResultOutbox   *repository.JudgeResultOutboxRelay

	UserContextMiddleware  rest.Middleware
	InternalAuthMiddleware rest.Middleware
	WorkerAuthMiddleware   rest.Middleware

	backgroundCancel context.CancelFunc
	backgroundWG     sync.WaitGroup
}

type WorkerTaskRepository interface {
	UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error)
	WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error)
	RecoverStaleTasks(ctx context.Context) (int64, error)
	ClaimTasks(ctx context.Context, workerID string, supportedLanguages []string, limit int, leaseTTL time.Duration, taskIDs []string) ([]repository.TaskLeaseView, error)
	RefreshClaimedTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error)
	ReleaseClaimedTasks(ctx context.Context, workerID string, leases []repository.TaskLeaseView, reason string) (int64, error)
	RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error)
	GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error)
	GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error)
	GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error)
	MarkTaskSucceeded(ctx context.Context, taskID string, workerID string, leaseVersion int, transition repository.TaskSuccessTransition) error
	MarkTaskFailed(ctx context.Context, taskID string, workerID string, leaseVersion int, transition repository.TaskFailureTransition) (repository.TaskFailureOutcome, error)
}

type SubmissionRepository interface {
	GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error)
	CreateSubmission(ctx context.Context, problemID int64, userID int64, language string) (int64, error)
	UpdateSubmissionSource(ctx context.Context, submissionID int64, codePath string, codeSha256 string, resultPath string) error
	EnsureTaskForSubmission(ctx context.Context, submissionID int64) error
	MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error
}

type PermissionChecker = sharedperm.UserChecker

func (s *ServiceContext) ActiveSubmissionRepo() SubmissionRepository {
	if s == nil {
		return nil
	}
	if s.SubmissionRepo != nil {
		return s.SubmissionRepo
	}
	return s.Repo
}

func (s *ServiceContext) ActivePermissionChecker() PermissionChecker {
	if s == nil {
		return nil
	}
	if s.Permission != nil {
		return s.Permission
	}
	return sharedperm.NewDatabaseUserChecker(s.DB)
}

func NewServiceContext(c config.Config) *ServiceContext {
	ctx := context.Background()
	applyEnvOverrides(&c)
	if token := os.Getenv("OJOS_WORKER_TOKEN"); token != "" {
		c.WorkerAuth.Token = token
	}
	if leaseTTL := os.Getenv("OJOS_TASK_LEASE_TTL"); leaseTTL != "" && c.WorkerAuth.LeaseTTLSeconds <= 0 {
		if parsed, err := time.ParseDuration(leaseTTL + "s"); err == nil {
			c.WorkerAuth.LeaseTTLSeconds = int64(parsed.Seconds())
		}
	}

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
		"judge-api",
		nil,
		[]eventing.EventSubscription{
			{EventType: eventing.ProblemDeletedV1, ConsumerGroup: "judge-api"},
			{EventType: eventing.ProblemSnapshotV1, ConsumerGroup: "judge-api"},
		},
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
		// Judge queue wakeups and event projection share the Agent-approved
		// Redis connection. A managed container never needs a release-provided
		// REDIS_URL or a control-plane/global Redis credential.
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
		"judge-api",
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

	repo := repository.New(db, repository.WithLegacyProblemPackageDir(c.ProblemProjection.AllowLegacyPackageDir))
	var workloadVerifier *workload.Verifier
	if strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) != "" {
		workloadVerifier, err = workload.NewVerifierFromPEMFile(
			c.WorkloadIdentity.PublicKeyFile,
			c.WorkloadIdentity.KeyID,
			c.WorkloadIdentity.Issuer,
			c.WorkloadIdentity.Audience,
		)
		if err != nil {
			log.Fatalf("configure workload identity verifier failed: %v", err)
		}
	}
	if err := validateWorkerIdentityMode(
		c,
		workloadVerifier != nil,
		os.Getenv("OJOS_ENVIRONMENT"),
	); err != nil {
		log.Fatalf("invalid Judge Worker identity configuration: %v", err)
	}
	if err := validateProblemProjectionMode(c, os.Getenv("OJOS_ENVIRONMENT")); err != nil {
		log.Fatalf("invalid Problem projection configuration: %v", err)
	}
	workerAuthOptions := []any{}
	if workloadVerifier != nil {
		workerAuthOptions = append(workerAuthOptions, workloadVerifier)
	}
	// Explicitly pass the compatibility decision even when no verifier is
	// configured. This keeps development opt-in possible without making the
	// middleware constructor permissive by default.
	workerAuthOptions = append(workerAuthOptions, c.WorkloadIdentity.AllowLegacyWorkerToken)
	resultStream := strings.TrimSpace(os.Getenv("OJOS_JUDGE_RESULT_STREAM"))
	if resultStream == "" {
		resultStream = "ojos:judge:result"
	}
	svcCtx := &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		Repo:           repo,
		SubmissionRepo: repo,
		WorkerRepo:     repo,
		Permission:     permissionChecker,
		Redis:          redisClient,
		Events:         eventContext,
		EventRedis:     eventRedis,
		ResultOutbox: &repository.JudgeResultOutboxRelay{
			DB:           db,
			Redis:        eventRedis,
			Stream:       resultStream,
			RelayID:      c.Name,
			PollInterval: 250 * time.Millisecond,
		},

		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		WorkerAuthMiddleware: middleware.NewWorkerAuthMiddleware(c.WorkerAuth.Token, workerAuthOptions...).Handle,
	}
	svcCtx.startProblemProjectionConsumer()
	return svcCtx
}

func validateWorkerIdentityMode(c config.Config, verifierConfigured bool, environment string) error {
	if !strings.EqualFold(strings.TrimSpace(environment), "production") {
		return nil
	}
	if !verifierConfigured {
		return fmt.Errorf("production requires OJOS_WORKLOAD_PUBLIC_KEY_FILE")
	}
	if c.WorkloadIdentity.AllowLegacyWorkerToken || strings.TrimSpace(c.WorkerAuth.Token) != "" {
		return fmt.Errorf("production forbids the legacy shared Worker token")
	}
	return nil
}

func validateProblemProjectionMode(c config.Config, environment string) error {
	if !c.ProblemProjection.AllowLegacyPackageDir {
		return nil
	}
	if !strings.EqualFold(strings.TrimSpace(environment), "development") {
		return fmt.Errorf("legacy package_dir submissions are allowed only when OJOS_ENVIRONMENT=development; complete Problem projection backfill/reconcile first")
	}
	return nil
}

func (s *ServiceContext) startProblemProjectionConsumer() {
	ctx, cancel := context.WithCancel(context.Background())
	s.backgroundCancel = cancel
	if s.ResultOutbox != nil {
		s.backgroundWG.Add(1)
		go func() {
			defer s.backgroundWG.Done()
			s.ResultOutbox.Run(ctx)
		}()
	}
	stream := ""
	group := ""
	if s.Events != nil {
		stream = s.Events.Stream
		var err error
		group, err = s.Events.ConsumerGroupFor(
			eventing.ProblemDeletedV1,
			eventing.ProblemSnapshotV1,
		)
		if err != nil {
			// The context was already checked against this exact Release contract
			// at startup; reaching this branch indicates local file corruption.
			s.Logger.Fatal("managed Event Contract consumer group is invalid", zap.Error(err))
		}
	} else {
		// Compatibility is intentionally limited to unmanaged development.
		stream = strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_STREAM"))
		if stream == "" {
			stream = "ojos:integration:problem:v1"
		}
		group = "judge-api.problem-projection.v1"
	}
	hostname, _ := os.Hostname()
	consumer := &eventing.Consumer{
		DB:           s.DB,
		Redis:        s.EventRedis,
		Stream:       stream,
		Group:        group,
		ConsumerName: fmt.Sprintf("%s-%s-%d", s.Config.Name, hostname, os.Getpid()),
		BatchSize:    100,
		ClaimIdle:    30 * time.Second,
		MaxAttempts:  5,
		Handler:      repository.ApplyProblemProjection,
	}
	s.backgroundWG.Add(1)
	go func() {
		defer s.backgroundWG.Done()
		consumer.Run(ctx)
	}()
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
	if value := firstEnv("JUDGE_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
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
	if value := strings.TrimSpace(os.Getenv("OJOS_SUBMISSIONS_ROOT")); value != "" {
		c.Storage.SubmissionsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_ENDPOINT")); value != "" {
		c.Storage.ServiceEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_INTERNAL_GATEWAY_ENDPOINT")); value != "" {
		c.Storage.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_GET_API_ID")); value != "" {
		c.Storage.GetApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_HEAD_API_ID")); value != "" {
		c.Storage.HeadApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SUBMISSIONS_BUCKET")); value != "" {
		c.Storage.Bucket = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.Storage.CallerService = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_NODE_ID")); value != "" {
		c.Storage.CallerNodeID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SERVICE_TOKEN")); value != "" {
		c.Storage.ServiceToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE")); value != "" {
		c.WorkloadIdentity.PublicKeyFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_KEY_ID")); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_ISSUER")); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_AUDIENCE")); value != "" {
		c.WorkloadIdentity.Audience = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_WORKER_TOKEN")); value != "" {
		c.WorkloadIdentity.AllowLegacyWorkerToken = value == "1" || strings.EqualFold(value, "true")
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_PROBLEM_PACKAGE_DIR")); value != "" {
		c.ProblemProjection.AllowLegacyPackageDir = value == "1" || strings.EqualFold(value, "true")
	}
	// Deliberately a dedicated variable rather than reusing
	// OJOS_INTERNAL_GATEWAY_ENDPOINT (which already drives the storage client):
	// switching the permission check onto the gateway also requires a service
	// credential and a service permission grant, so it must be an explicit
	// opt-in per deployment.
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
	if value := firstEnv("OJOS_JUDGE_API_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.AuthService.ServiceToken = value
	}
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
