package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/handler"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	sharedperm "ojos-shared/security/permission"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
	"go.uber.org/zap"
)

type smokeRepo struct {
	mu               sync.Mutex
	nextSubmissionID int64
	problem          repository.ProblemMeta
	submissions      map[int64]*repository.SubmissionView
	leases           map[string]repository.TaskLeaseView
	workers          map[string]repository.WorkerView
}

func main() {
	var (
		host                    = flag.String("host", envDefault("OJOS_SMOKE_JUDGE_HOST", "127.0.0.1"), "listen host")
		port                    = flag.Int("port", envIntDefault("OJOS_SMOKE_JUDGE_PORT", 18082), "listen port")
		redisURL                = flag.String("redis", envDefault("REDIS_URL", "redis://127.0.0.1:6379/0"), "redis URL")
		storageRoot             = flag.String("submissions-root", envDefault("OJOS_SUBMISSIONS_ROOT", filepath.Join(os.TempDir(), "ojos-smoke-submissions")), "temporary submissions root")
		internalGatewayEndpoint = flag.String("internal-gateway", envDefault("OJOS_INTERNAL_GATEWAY_ENDPOINT", "http://127.0.0.1:18080"), "gateway internal resolver endpoint")
		workerToken             = flag.String("worker-token", envDefault("OJOS_WORKER_TOKEN", "ojos-smoke-worker"), "worker token")
		serviceToken            = flag.String("service-token", envDefault("OJOS_SERVICE_TOKEN", "ojos-smoke-internal"), "service identity token")
		callerNodeID            = flag.String("caller-node-id", envDefault("OJOS_CALLER_NODE_ID", "child-node"), "caller node id")
		problemPackageDir       = flag.String("problem-package", envDefault("OJOS_SMOKE_PROBLEM_PACKAGE", ""), "problem package directory")
	)
	flag.Parse()

	if strings.TrimSpace(*problemPackageDir) == "" {
		dir, err := createProblemPackage()
		if err != nil {
			log.Fatalf("create smoke problem package failed: %v", err)
		}
		*problemPackageDir = dir
	}
	if err := os.MkdirAll(*storageRoot, 0o755); err != nil {
		log.Fatalf("create submissions root failed: %v", err)
	}

	redisOptions, err := redis.ParseURL(*redisURL)
	if err != nil {
		log.Fatalf("parse redis url failed: %v", err)
	}
	redisClient := redis.NewClient(redisOptions)
	if err := redisClient.Ping(context.Background()).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
	}

	repo := &smokeRepo{
		nextSubmissionID: 1,
		problem: repository.ProblemMeta{
			ID:         1001,
			PackageDir: *problemPackageDir,
			Status:     "ready",
			Visibility: "public",
		},
		submissions: map[int64]*repository.SubmissionView{},
		leases:      map[string]repository.TaskLeaseView{},
		workers:     map[string]repository.WorkerView{},
	}
	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			RestConf: rest.RestConf{
				Host: *host,
				Port: *port,
			},
			Storage: config.StorageConfig{
				SubmissionsRoot:         *storageRoot,
				InternalGatewayEndpoint: strings.TrimRight(*internalGatewayEndpoint, "/"),
				GetApiID:                "storage.object.get",
				PutApiID:                "storage.object.put",
				HeadApiID:               "storage.object.head",
				Bucket:                  "submissions",
				CallerService:           "judge-api",
				CallerNodeID:            *callerNodeID,
				ServiceToken:            *serviceToken,
			},
			Submission: config.SubmissionConfig{MaxCodeBytes: 262144},
			Languages: config.LanguagesConfig{Items: []config.LanguageConfig{
				{Id: "cpp17", DisplayName: "C++17", Version: "smoke", Enabled: true, SourceFile: "main.cpp"},
				{Id: "c11", DisplayName: "C11", Version: "smoke", Enabled: true, SourceFile: "main.c"},
				{Id: "python3", DisplayName: "Python 3", Version: "smoke", Enabled: true, SourceFile: "main.py"},
				{Id: "java17", DisplayName: "Java 17", Version: "smoke", Enabled: true, SourceFile: "Main.java"},
			}},
			WorkerAuth: config.WorkerAuthConfig{
				Token:           *workerToken,
				LeaseTTLSeconds: 60,
			},
		},
		Logger:                 zap.NewNop(),
		SubmissionRepo:         repo,
		WorkerRepo:             repo,
		Permission:             allowAllPermissionChecker{},
		Redis:                  redisClient,
		UserContextMiddleware:  middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(false, nil).Handle,
		WorkerAuthMiddleware:   middleware.NewWorkerAuthMiddleware(*workerToken).Handle,
	}

	server := rest.MustNewServer(svcCtx.Config.RestConf)
	defer server.Stop()
	handler.RegisterHandlers(server, svcCtx)

	log.Printf("Starting judge-api smoke server at %s:%d", *host, *port)
	server.Start()
}

type allowAllPermissionChecker struct{}

func (allowAllPermissionChecker) RequireUserPermission(context.Context, int64, string, sharedperm.Scope) error {
	return nil
}

func (allowAllPermissionChecker) HasUserPermission(context.Context, int64, string, sharedperm.Scope) (bool, error) {
	return true, nil
}

func (r *smokeRepo) GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if id != r.problem.ID {
		return nil, errors.New("problem not found")
	}
	copied := r.problem
	return &copied, nil
}

func (r *smokeRepo) CreateSubmission(ctx context.Context, problemID int64, userID int64, language string) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	id := r.nextSubmissionID
	r.nextSubmissionID++
	now := time.Now()
	r.submissions[id] = &repository.SubmissionView{
		ID:        id,
		ProblemID: problemID,
		UserID:    userID,
		Language:  language,
		Status:    "PENDING",
		CreatedAt: now,
		UpdatedAt: now,
	}
	return id, nil
}

func (r *smokeRepo) UpdateSubmissionSource(ctx context.Context, submissionID int64, codePath string, codeSha256 string, resultPath string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	submission, ok := r.submissions[submissionID]
	if !ok {
		return repository.ErrSubmissionNotFound
	}
	submission.CodePath = codePath
	submission.CodeSha256 = codeSha256
	submission.ResultPath = resultPath
	submission.UpdatedAt = time.Now()
	return nil
}

func (r *smokeRepo) EnsureTaskForSubmission(ctx context.Context, submissionID int64) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	submission, ok := r.submissions[submissionID]
	if !ok {
		return repository.ErrSubmissionNotFound
	}
	taskID := fmt.Sprintf("sub-%d", submissionID)
	r.leases[taskID] = repository.TaskLeaseView{
		TaskID:       taskID,
		SubmissionID: submission.ID,
		ProblemID:    submission.ProblemID,
		Language:     submission.Language,
		Status:       "PENDING",
	}
	return nil
}

func (r *smokeRepo) MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if submission, ok := r.submissions[submissionID]; ok {
		submission.Status = "SYSTEM_ERROR"
		submission.Message = message
		submission.UpdatedAt = time.Now()
	}
	return nil
}

func (r *smokeRepo) UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if w.MaxConcurrency <= 0 {
		w.MaxConcurrency = 1
	}
	now := time.Now()
	view := repository.WorkerView{
		WorkerID:           w.WorkerID,
		WorkerName:         w.WorkerName,
		Hostname:           w.Hostname,
		Version:            w.Version,
		Capabilities:       append([]string(nil), w.Capabilities...),
		SupportedLanguages: append([]string(nil), w.SupportedLanguages...),
		MaxConcurrency:     w.MaxConcurrency,
		Status:             "ONLINE",
		LastSeen:           now,
		RegisteredAt:       now,
		UpdatedAt:          now,
	}
	r.workers[w.WorkerID] = view
	return &view, nil
}

func (r *smokeRepo) WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	view, ok := r.workers[workerID]
	if !ok {
		return nil, repository.ErrWorkerNotFound
	}
	view.RunningCount = runningCount
	view.LastSeen = time.Now()
	view.UpdatedAt = time.Now()
	r.workers[workerID] = view
	return &view, nil
}

func (r *smokeRepo) RecoverStaleTasks(ctx context.Context) (int64, error) {
	return 0, nil
}

func (r *smokeRepo) ClaimTasks(ctx context.Context, workerID string, supportedLanguages []string, limit int, leaseTTL time.Duration, taskIDs []string) ([]repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, ok := r.workers[workerID]; !ok {
		return nil, repository.ErrWorkerNotFound
	}
	if limit <= 0 {
		return nil, nil
	}
	claimed := make([]repository.TaskLeaseView, 0, limit)
	for _, taskID := range taskIDs {
		lease, ok := r.leases[taskID]
		if !ok || lease.Status != "PENDING" || !languageSupported(supportedLanguages, lease.Language) {
			continue
		}
		lease.WorkerID = workerID
		lease.LeaseVersion++
		lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
		lease.HeartbeatAt = time.Now()
		lease.Attempt++
		lease.Status = "RUNNING"
		r.leases[taskID] = lease
		if submission, ok := r.submissions[lease.SubmissionID]; ok {
			submission.Status = "JUDGING"
			submission.UpdatedAt = time.Now()
		}
		claimed = append(claimed, lease)
		if len(claimed) >= limit {
			break
		}
	}
	return claimed, nil
}

func (r *smokeRepo) RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return nil, repository.ErrTaskLeaseInvalid
	}
	lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
	lease.HeartbeatAt = time.Now()
	r.leases[taskID] = lease
	return &lease, nil
}

func (r *smokeRepo) GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return nil, repository.ErrTaskLeaseInvalid
	}
	copied := lease
	return &copied, nil
}

func (r *smokeRepo) GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	submission, ok := r.submissions[id]
	if !ok {
		return nil, repository.ErrSubmissionNotFound
	}
	copied := *submission
	return &copied, nil
}

func (r *smokeRepo) MarkTaskSucceeded(ctx context.Context, taskID string, workerID string, leaseVersion int, status string, score int, timeMS int, memoryKB int, message string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return repository.ErrTaskLeaseInvalid
	}
	lease.Status = "SUCCEEDED"
	r.leases[taskID] = lease
	if submission, ok := r.submissions[lease.SubmissionID]; ok {
		now := time.Now()
		submission.Status = status
		submission.Score = score
		submission.TimeMS = timeMS
		submission.MemoryKB = memoryKB
		submission.Message = message
		submission.JudgedAt = &now
		submission.UpdatedAt = now
	}
	return nil
}

func (r *smokeRepo) MarkTaskFailed(ctx context.Context, taskID string, workerID string, leaseVersion int, status string, message string, retryable bool) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return repository.ErrTaskLeaseInvalid
	}
	if retryable {
		lease.Status = "PENDING"
	} else {
		lease.Status = "FAILED"
	}
	r.leases[taskID] = lease
	if submission, ok := r.submissions[lease.SubmissionID]; ok {
		submission.Status = status
		submission.Message = message
		submission.UpdatedAt = time.Now()
	}
	return nil
}

func languageSupported(items []string, value string) bool {
	if len(items) == 0 {
		return true
	}
	for _, item := range items {
		if strings.TrimSpace(item) == value {
			return true
		}
	}
	return false
}

func createProblemPackage() (string, error) {
	dir, err := os.MkdirTemp("", "ojos-smoke-problem-*")
	if err != nil {
		return "", err
	}
	if err := os.WriteFile(filepath.Join(dir, "manifest.json"), []byte(`{"cases":[1]}`), 0o644); err != nil {
		return "", err
	}
	return dir, nil
}

func envDefault(key string, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}

func envIntDefault(key string, fallback int) int {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		var parsed int
		if _, err := fmt.Sscanf(value, "%d", &parsed); err == nil {
			return parsed
		}
	}
	return fallback
}
