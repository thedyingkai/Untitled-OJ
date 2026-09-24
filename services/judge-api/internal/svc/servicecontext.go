// Initialized dependencies and service-lifetime cleanup.
package svc

import (
	"context"
	"sync"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
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
