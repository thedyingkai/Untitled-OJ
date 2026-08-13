package svc

import (
	"context"
	"errors"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-problem-events/problemv1"
	"ojos-shared/eventing"
	"ojos-shared/resourceoutput"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/security/workload"
	"ojos-shared/servicecontext"

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
	RejudgeRepo    RejudgeRepository
	WorkerRepo     WorkerTaskRepository
	Permission     PermissionChecker
	Redis          *redis.Client
	Events         *eventing.EventContext
	EventRedis     redis.UniversalClient
	ResultOutbox   *repository.JudgeResultOutboxRelay
	Context        *servicecontext.ContextProvider
	Managed        bool

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

type RejudgeRepository interface {
	GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error)
	ResetSubmissionsForProblem(ctx context.Context, problemID int64) ([]int64, error)
	EnsureTaskForSubmission(ctx context.Context, submissionID int64) error
}

type PermissionChecker = sharedperm.UserChecker

const redisStartupProbeTimeout = 750 * time.Millisecond

const (
	permissionBindingName        = sharedperm.DefaultPermissionCheckApiID
	storageGetBinding            = "storage.object.get"
	storagePutBinding            = "storage.object.put"
	storageHeadBinding           = "storage.object.head"
	defaultSubmissionsOutputFile = "/run/ojos/resources/submissions/dsn"
)

func (s *ServiceContext) ActiveSubmissionRepo() SubmissionRepository {
	if s == nil {
		return nil
	}
	if s.SubmissionRepo != nil {
		return s.SubmissionRepo
	}
	return s.Repo
}

func (s *ServiceContext) ActiveRejudgeRepo() RejudgeRepository {
	if s == nil {
		return nil
	}
	if s.RejudgeRepo != nil {
		return s.RejudgeRepo
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
	if err := applyEnvOverrides(&c); err != nil {
		log.Fatalf("configure judge-api: %v", err)
	}
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
			{EventType: problemv1.DeletedType, ConsumerGroup: "judge-api"},
			{EventType: problemv1.SnapshotType, ConsumerGroup: "judge-api"},
		},
	)
	if err != nil {
		log.Fatalf("configure managed Event Contract failed: %v", err)
	}
	redisClient, err := newJudgeRedisClient(eventContext, c.Redis.Url)
	if err != nil {
		log.Fatalf("configure judge redis client failed: %v", err)
	}
	if pingErr := probeJudgeRedis(ctx, redisClient); pingErr != nil {
		// Redis is an acceleration and event transport dependency, not the
		// durable Judge task authority. Starting without connectivity lets the
		// API persist submissions and lets Workers poll PostgreSQL; the existing
		// consumers and relays retry Redis in the background.
		zlog.Warn(
			"redis unavailable at startup; continuing with PostgreSQL judge task polling",
			zap.Error(pingErr),
		)
	}
	var eventRedis redis.UniversalClient = redisClient
	var contextProvider *servicecontext.ContextProvider
	var permissionChecker sharedperm.UserChecker
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		log.Fatalf("load managed Service Context failed: %v", err)
	}
	if contextValue != nil {
		if err := contextValue.RequireService("judge-api"); err != nil {
			log.Fatalf("validate managed Service Context failed: %v", err)
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			for _, bindingName := range []string{storageGetBinding, storagePutBinding, storageHeadBinding} {
				var binding servicecontext.APIBinding
				binding, err = contextProvider.Binding(ctx, bindingName)
				if err != nil || binding.APIID != bindingName {
					err = fmt.Errorf("required API binding %s is unavailable", bindingName)
					break
				}
			}
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			_ = contextProvider.Close()
			log.Fatalf("configure managed ApiBindings failed: %v", err)
		}
	} else {
		if managedEnvironment() {
			log.Fatal("managed judge-api requires an Agent Service Context")
		}
		permissionChecker = sharedperm.NewUserCheckerWithConfig(permissionCheckerConfig(c), db)
		if permissionChecker == nil {
			log.Fatal("configure permission checker failed")
		}
	}
	c.Storage.SetContextProvider(contextProvider)

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
		Config:  c,
		Managed: managedEnvironment(),

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		Repo:           repo,
		SubmissionRepo: repo,
		RejudgeRepo:    repo,
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
		Context: contextProvider,

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

func newJudgeRedisClient(eventContext *eventing.EventContext, redisURL string) (*redis.Client, error) {
	if eventContext != nil {
		// Judge queue wakeups and event projection share the Agent-approved
		// Redis connection. RedisClient validates the materialized connection
		// file and URL without requiring the endpoint to be reachable yet.
		client, err := eventContext.RedisClient()
		if err != nil {
			return nil, fmt.Errorf("load Agent-local event connection: %w", err)
		}
		return client, nil
	}

	redisURL = strings.TrimSpace(redisURL)
	if redisURL == "" {
		return nil, errors.New("redis url is required")
	}
	options, err := redis.ParseURL(redisURL)
	if err != nil {
		return nil, fmt.Errorf("parse redis url: %w", err)
	}
	return redis.NewClient(options), nil
}

func probeJudgeRedis(ctx context.Context, client *redis.Client) error {
	if client == nil {
		return errors.New("redis client is not configured")
	}
	probeCtx, cancel := context.WithTimeout(ctx, redisStartupProbeTimeout)
	defer cancel()
	return client.Ping(probeCtx).Err()
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
	var transport eventing.TransportConfig
	if s.Events != nil {
		var err error
		transport, err = s.Events.SubscriberTransport(
			problemv1.DeletedType,
			problemv1.SnapshotType,
		)
		if err != nil {
			// The context was already checked against this exact Release contract
			// at startup; reaching this branch indicates local file corruption.
			s.Logger.Fatal("managed Event Contract consumer group is invalid", zap.Error(err))
		}
	} else {
		// Compatibility is intentionally limited to unmanaged development.
		stream := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_STREAM"))
		if stream == "" {
			stream = eventing.DefaultEventStream
		}
		group := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_CONSUMER_GROUP"))
		if group == "" {
			group = s.Config.Name
		}
		transport = eventing.DevelopmentSubscriberTransport(stream, group)
	}
	hostname, _ := os.Hostname()
	consumer, err := eventing.NewConsumer(s.DB, s.EventRedis, transport, repository.ApplyProblemProjection)
	if err != nil {
		s.Logger.Fatal("configure problem projection consumer failed", zap.Error(err))
	}
	consumer.ConsumerName = fmt.Sprintf("%s-%s-%d", s.Config.Name, hostname, os.Getpid())
	consumer.BatchSize = 100
	consumer.ClaimIdle = 30 * time.Second
	consumer.MaxAttempts = eventing.DefaultMaxAttempts
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

func applyEnvOverrides(c *config.Config) error {
	managed := managedEnvironment()
	if managed {
		path := firstEnv("OJOS_RESOURCE_SUBMISSIONS_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultSubmissionsOutputFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load submissions resource output: %w", err)
		}
		c.Database.Url = dsn
	} else if value := firstEnv("JUDGE_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if !managed {
		// Direct URLs and long-lived service tokens are development-only escape
		// hatches. A managed workload receives endpoints, TLS roots and rotated
		// credentials exclusively through Agent materialization.
		if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
			c.Redis.Url = value
		}
		if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
			c.AuthService.Endpoint = value
		}
		if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
			c.AuthService.AdminToken = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_ENDPOINT")); value != "" {
			c.Storage.ServiceEndpoint = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_INTERNAL_GATEWAY_ENDPOINT")); value != "" {
			c.Storage.InternalGatewayEndpoint = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_NODE_ID")); value != "" {
			c.Storage.CallerNodeID = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_SERVICE_TOKEN")); value != "" {
			c.Storage.ServiceToken = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT")); value != "" {
			c.AuthService.InternalGatewayEndpoint = value
		}
		if value := firstEnv(
			"OJOS_AUTH_PERMISSION_CALLER_NODE_ID",
			"OJOS_CALLER_NODE_ID",
			"OJOS_NODE_ID",
		); value != "" {
			c.AuthService.CallerNodeID = value
		}
		if value := firstEnv("OJOS_JUDGE_API_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
			c.AuthService.ServiceToken = value
		}
	} else {
		// Clear values supplied by a legacy configuration file as well as env.
		c.Redis.Url = ""
		c.AuthService.Endpoint = ""
		c.AuthService.AdminToken = ""
		c.AuthService.InternalGatewayEndpoint = ""
		c.AuthService.CallerNodeID = ""
		c.AuthService.ServiceToken = ""
		c.Storage.ServiceEndpoint = ""
		c.Storage.InternalGatewayEndpoint = ""
		c.Storage.CallerNodeID = ""
		c.Storage.ServiceToken = ""
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SUBMISSIONS_ROOT")); value != "" && !managed {
		c.Storage.SubmissionsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_GET_API_ID")); value != "" && !managed {
		c.Storage.GetApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" && !managed {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_HEAD_API_ID")); value != "" && !managed {
		c.Storage.HeadApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SUBMISSIONS_BUCKET")); value != "" {
		c.Storage.Bucket = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_SUBMISSION_MAXCODEBYTES")); value != "" {
		parsed, err := strconv.ParseInt(value, 10, 64)
		if err != nil || parsed < 1024 || parsed > 10*1024*1024 {
			return errors.New("OJOS_CONFIG_SUBMISSION_MAXCODEBYTES is invalid")
		}
		c.Submission.MaxCodeBytes = parsed
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKER_LEASETTLSECONDS")); value != "" {
		parsed, err := strconv.ParseInt(value, 10, 64)
		if err != nil || parsed < 10 || parsed > 3600 {
			return errors.New("OJOS_CONFIG_WORKER_LEASETTLSECONDS is invalid")
		}
		c.WorkerAuth.LeaseTTLSeconds = parsed
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" && !managed {
		c.Storage.CallerService = value
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
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_WORKER_TOKEN")); value != "" && !managed {
		c.WorkloadIdentity.AllowLegacyWorkerToken = value == "1" || strings.EqualFold(value, "true")
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_PROBLEM_PACKAGE_DIR")); value != "" && !managed {
		c.ProblemProjection.AllowLegacyPackageDir = value == "1" || strings.EqualFold(value, "true")
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CHECK_API_ID")); value != "" && !managed {
		c.AuthService.PermissionCheckApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CALLER_SERVICE")); value != "" && !managed {
		c.AuthService.CallerService = value
	}
	return nil
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true") ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil || s.DB.Ping(ctx) != nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if s.EventRedis == nil || s.EventRedis.Ping(ctx).Err() != nil {
		return errors.New("event transport is unavailable")
	}
	if s.Context == nil {
		if managedEnvironment() {
			return errors.New("managed Service Context is unavailable")
		}
		return nil
	}
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil || snapshot.RequireService("judge-api") != nil {
		return errors.New("managed Service Context is invalid")
	}
	required := map[string]string{
		permissionBindingName: sharedperm.DefaultPermissionCheckApiID,
		storageGetBinding:     storageGetBinding,
		storagePutBinding:     storagePutBinding,
		storageHeadBinding:    storageHeadBinding,
	}
	for name, apiID := range required {
		binding, bindingErr := snapshot.Binding(name)
		if bindingErr != nil || binding.APIID != apiID {
			return fmt.Errorf("required API binding %s is unavailable", name)
		}
	}
	if _, err := snapshot.Client(); err != nil {
		return errors.New("required API client is unavailable")
	}
	if _, err := s.Context.Credential(ctx); err != nil {
		return errors.New("workload credential is unavailable")
	}
	return nil
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
