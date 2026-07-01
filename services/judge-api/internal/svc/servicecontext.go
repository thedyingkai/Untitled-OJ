package svc

import (
	"context"
	"log"
	"os"
	"strings"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"

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

	UserContextMiddleware  rest.Middleware
	InternalAuthMiddleware rest.Middleware
	WorkerAuthMiddleware   rest.Middleware
}

type WorkerTaskRepository interface {
	UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error)
	WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error)
	RecoverStaleTasks(ctx context.Context) (int64, error)
	ClaimTasks(ctx context.Context, workerID string, supportedLanguages []string, limit int, leaseTTL time.Duration, taskIDs []string) ([]repository.TaskLeaseView, error)
	RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error)
	GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error)
	GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error)
	GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error)
	MarkTaskSucceeded(ctx context.Context, taskID string, workerID string, leaseVersion int, status string, score int, timeMS int, memoryKB int, message string) error
	MarkTaskFailed(ctx context.Context, taskID string, workerID string, leaseVersion int, status string, message string, retryable bool) error
}

type SubmissionRepository interface {
	GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error)
	CreateSubmission(ctx context.Context, problemID int64, userID int64, language string) (int64, error)
	UpdateSubmissionSource(ctx context.Context, submissionID int64, codePath string, codeSha256 string, resultPath string) error
	EnsureTaskForSubmission(ctx context.Context, submissionID int64) error
	MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error
}

type PermissionChecker interface {
	RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error
	HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) (bool, error)
}

type databasePermissionChecker struct {
	db *pgxpool.Pool
}

func (p databasePermissionChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	return sharedperm.RequireUserPermission(ctx, p.db, userID, permissionCode, scope)
}

func (p databasePermissionChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) (bool, error) {
	return sharedperm.HasUserPermission(ctx, p.db, userID, permissionCode, scope)
}

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
	if s.DB == nil {
		return nil
	}
	return databasePermissionChecker{db: s.DB}
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

	redisOptions, err := redis.ParseURL(c.Redis.Url)
	if err != nil {
		log.Fatalf("parse redis url failed: %v", err)
	}

	redisClient := redis.NewClient(redisOptions)
	if err := redisClient.Ping(ctx).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
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

	repo := repository.New(db)
	return &ServiceContext{
		Config: c,

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		Repo:           repo,
		SubmissionRepo: repo,
		WorkerRepo:     repo,
		Permission:     databasePermissionChecker{db: db},
		Redis:          redisClient,

		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		WorkerAuthMiddleware: middleware.NewWorkerAuthMiddleware(c.WorkerAuth.Token).Handle,
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
