package logic

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

func TestClaimRefreshesAfterMetadataBuildExceedsInitialTTL(t *testing.T) {
	metadataStarted := make(chan struct{}, 1)
	storage := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		select {
		case metadataStarted <- struct{}{}:
		default:
		}
		time.Sleep(1100 * time.Millisecond)
		writeClaimMetadata(t, w)
	}))
	defer storage.Close()

	repo := newClaimLeaseRepo(claimLeaseFixture("sub-slow", 81))
	svcCtx := claimLeaseServiceContext(repo, storage.URL, 1)
	started := time.Now()
	resp, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasks(&types.WorkerClaimTasksReq{
		WorkerId:           "worker-a",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     1,
		TaskIds:            []string{"sub-slow"},
	})
	if err != nil {
		t.Fatalf("claim after slow metadata: %v", err)
	}
	if len(resp.Tasks) != 1 {
		t.Fatalf("expected one lease, got %#v", resp.Tasks)
	}
	if time.Since(started) <= time.Second {
		t.Fatal("fixture did not outlive the initial lease TTL")
	}
	expiresAt, err := time.Parse(time.RFC3339Nano, resp.Tasks[0].LeaseExpiresAt)
	if err != nil {
		t.Fatalf("parse refreshed expiry: %v", err)
	}
	if !expiresAt.After(time.Now()) {
		t.Fatalf("claim returned an expired lease: %s", expiresAt)
	}
	if repo.refreshCount() != 1 {
		t.Fatalf("response lease was not finalized exactly once: %d", repo.refreshCount())
	}
}

func TestClaimMetadataFailureImmediatelyReleasesForReclaim(t *testing.T) {
	var fail atomic.Bool
	fail.Store(true)
	storage := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if fail.Load() {
			http.Error(w, "metadata unavailable", http.StatusServiceUnavailable)
			return
		}
		writeClaimMetadata(t, w)
	}))
	defer storage.Close()

	repo := newClaimLeaseRepo(claimLeaseFixture("sub-retry", 82))
	svcCtx := claimLeaseServiceContext(repo, storage.URL, 30)
	request := &types.WorkerClaimTasksReq{
		WorkerId:           "worker-a",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     1,
		TaskIds:            []string{"sub-retry"},
	}
	if _, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasks(request); err == nil {
		t.Fatal("metadata failure unexpectedly returned a lease")
	}
	afterFailure := repo.lease("sub-retry")
	if afterFailure.Status != "PENDING" || afterFailure.WorkerID != "" || !afterFailure.LeaseExpiresAt.IsZero() {
		t.Fatalf("failed response stranded a running lease: %#v", afterFailure)
	}
	if afterFailure.Attempt != 0 {
		t.Fatalf("an unexposed claim consumed an execution attempt: %d", afterFailure.Attempt)
	}

	fail.Store(false)
	resp, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasks(request)
	if err != nil {
		t.Fatalf("immediate reclaim failed: %v", err)
	}
	if len(resp.Tasks) != 1 || resp.Tasks[0].TaskId != "sub-retry" {
		t.Fatalf("released task was not immediately reclaimable: %#v", resp)
	}
}

func TestClaimFinalizeFailureCompensationDoesNotOverwriteNewLease(t *testing.T) {
	storage := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		writeClaimMetadata(t, w)
	}))
	defer storage.Close()

	repo := newClaimLeaseRepo(
		claimLeaseFixture("sub-old", 83),
		claimLeaseFixture("sub-replaced", 84),
	)
	repo.failRefreshTask = "sub-replaced"
	repo.replaceFailedRefresh = true
	svcCtx := claimLeaseServiceContext(repo, storage.URL, 30)
	resp, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasks(&types.WorkerClaimTasksReq{
		WorkerId:           "worker-a",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     2,
		TaskIds:            []string{"sub-old", "sub-replaced"},
	})
	if err == nil || resp != nil {
		t.Fatalf("partial lease batch leaked to caller: resp=%#v err=%v", resp, err)
	}
	released := repo.lease("sub-old")
	if released.Status != "PENDING" || released.WorkerID != "" {
		t.Fatalf("still-owned lease was not compensated: %#v", released)
	}
	replaced := repo.lease("sub-replaced")
	if replaced.Status != "RUNNING" || replaced.WorkerID != "worker-b" || replaced.LeaseVersion != 2 {
		t.Fatalf("stale compensation overwrote a newer lease: %#v", replaced)
	}
}

func TestWorkerClaimsDatabaseTaskWithoutRedisTaskID(t *testing.T) {
	storage := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		writeClaimMetadata(t, w)
	}))
	defer storage.Close()

	repo := newClaimLeaseRepo(claimLeaseFixture("sub-db-only", 89))
	svcCtx := claimLeaseServiceContext(repo, storage.URL, 30)
	resp, err := NewWorkerClaimTasksLogic(context.Background(), svcCtx).WorkerClaimTasks(&types.WorkerClaimTasksReq{
		WorkerId:           "worker-a",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     1,
		TaskIds:            nil,
	})
	if err != nil {
		t.Fatalf("claim PostgreSQL task without Redis signal: %v", err)
	}
	if len(resp.Tasks) != 1 || resp.Tasks[0].TaskId != "sub-db-only" {
		t.Fatalf("database polling did not claim pending task: %#v", resp)
	}
}

func TestWorkerArtifactHandlersRejectExpiredRunningLease(t *testing.T) {
	fixture := claimLeaseFixture("sub-expired", 85)
	fixture.WorkerID = "worker-a"
	fixture.LeaseVersion = 4
	fixture.Status = "RUNNING"
	fixture.LeaseExpiresAt = time.Now().Add(-time.Millisecond)
	repo := newClaimLeaseRepo(fixture)
	svcCtx := &svc.ServiceContext{WorkerRepo: repo}

	sourceErr := ServeWorkerSubmissionSource(
		context.Background(),
		svcCtx,
		httptest.NewRecorder(),
		httptest.NewRequest(http.MethodGet, "/source", nil),
		&types.WorkerArtifactSourceReq{Id: 85, TaskId: fixture.TaskID, WorkerId: "worker-a", LeaseVersion: 4},
	)
	if sourceErr == nil || !strings.Contains(sourceErr.Error(), "lease does not match") {
		t.Fatalf("expired source artifact lease was accepted: %v", sourceErr)
	}

	problemErr := ServeWorkerProblemPackage(
		context.Background(),
		svcCtx,
		httptest.NewRecorder(),
		httptest.NewRequest(http.MethodGet, "/package", nil),
		&types.WorkerArtifactProblemPackageReq{Id: fixture.ProblemID, TaskId: fixture.TaskID, WorkerId: "worker-a", LeaseVersion: 4},
	)
	if problemErr == nil || !strings.Contains(problemErr.Error(), "lease does not match") {
		t.Fatalf("expired problem artifact lease was accepted: %v", problemErr)
	}
}

func writeClaimMetadata(t *testing.T, w http.ResponseWriter) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(storageObjectMetadata{
		Bucket:      "submissions",
		Key:         "source.cpp",
		SizeBytes:   24,
		SHA256:      strings.Repeat("a", 64),
		ContentType: "text/plain; charset=utf-8",
	}); err != nil {
		t.Errorf("encode metadata: %v", err)
	}
}

func claimLeaseFixture(taskID string, submissionID int64) repository.TaskLeaseView {
	return repository.TaskLeaseView{
		TaskID:       taskID,
		SubmissionID: submissionID,
		ProblemID:    submissionID + 1000,
		Language:     "cpp17",
		Status:       "PENDING",
	}
}

func claimLeaseServiceContext(repo *claimLeaseRepo, storageEndpoint string, leaseTTLSeconds int64) *svc.ServiceContext {
	return &svc.ServiceContext{
		Config: config.Config{
			Storage: config.StorageConfig{
				ServiceEndpoint: storageEndpoint,
				Bucket:          "submissions",
			},
			WorkerAuth: config.WorkerAuthConfig{LeaseTTLSeconds: leaseTTLSeconds},
			WorkloadIdentity: config.WorkloadIdentityConfig{
				AllowLegacyWorkerToken: true,
			},
		},
		WorkerRepo: repo,
	}
}

type claimLeaseRepo struct {
	mu                   sync.Mutex
	leases               map[string]repository.TaskLeaseView
	submissions          map[int64]*repository.SubmissionView
	refreshes            int
	failRefreshTask      string
	replaceFailedRefresh bool
}

func newClaimLeaseRepo(fixtures ...repository.TaskLeaseView) *claimLeaseRepo {
	repo := &claimLeaseRepo{
		leases:      make(map[string]repository.TaskLeaseView, len(fixtures)),
		submissions: make(map[int64]*repository.SubmissionView, len(fixtures)),
	}
	for i := range fixtures {
		lease := fixtures[i]
		repo.leases[lease.TaskID] = lease
		repo.submissions[lease.SubmissionID] = &repository.SubmissionView{
			ID:                       lease.SubmissionID,
			ProblemID:                lease.ProblemID,
			Language:                 lease.Language,
			CodePath:                 "storage://submissions/source.cpp",
			ProblemArtifactURI:       "storage://problems/package.zip",
			ProblemArtifactSHA256:    strings.Repeat("b", 64),
			ProblemArtifactSizeBytes: 128,
		}
	}
	return repo
}

func (r *claimLeaseRepo) lease(taskID string) repository.TaskLeaseView {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.leases[taskID]
}

func (r *claimLeaseRepo) refreshCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.refreshes
}

func (r *claimLeaseRepo) UpsertWorker(context.Context, repository.WorkerRegistration) (*repository.WorkerView, error) {
	return &repository.WorkerView{}, nil
}

func (r *claimLeaseRepo) WorkerHeartbeat(context.Context, string, int) (*repository.WorkerView, error) {
	return &repository.WorkerView{}, nil
}

func (r *claimLeaseRepo) RecoverStaleTasks(context.Context) (int64, error) { return 0, nil }

func (r *claimLeaseRepo) ClaimTasks(_ context.Context, workerID string, _ []string, limit int, leaseTTL time.Duration, taskIDs []string) ([]repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	claimed := make([]repository.TaskLeaseView, 0, limit)
	if len(taskIDs) == 0 {
		taskIDs = make([]string, 0, len(r.leases))
		for taskID := range r.leases {
			taskIDs = append(taskIDs, taskID)
		}
	}
	for _, taskID := range taskIDs {
		lease, ok := r.leases[taskID]
		if !ok || lease.Status != "PENDING" {
			continue
		}
		lease.WorkerID = workerID
		lease.LeaseVersion++
		lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
		lease.Status = "RUNNING"
		lease.Attempt++
		r.leases[taskID] = lease
		claimed = append(claimed, lease)
		if len(claimed) == limit {
			break
		}
	}
	return claimed, nil
}

func (r *claimLeaseRepo) RefreshClaimedTaskLease(_ context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if r.failRefreshTask == taskID {
		if r.replaceFailedRefresh && ok {
			lease.WorkerID = "worker-b"
			lease.LeaseVersion++
			lease.Status = "RUNNING"
			lease.LeaseExpiresAt = time.Now().Add(time.Minute)
			r.leases[taskID] = lease
		}
		return nil, repository.ErrTaskLeaseInvalid
	}
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return nil, repository.ErrTaskLeaseInvalid
	}
	lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
	lease.HeartbeatAt = time.Now()
	r.leases[taskID] = lease
	r.refreshes++
	copy := lease
	return &copy, nil
}

func (r *claimLeaseRepo) ReleaseClaimedTasks(_ context.Context, workerID string, leases []repository.TaskLeaseView, _ string) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	var released int64
	for i := range leases {
		current, ok := r.leases[leases[i].TaskID]
		if !ok || current.WorkerID != workerID || current.LeaseVersion != leases[i].LeaseVersion || current.Status != "RUNNING" {
			continue
		}
		current.WorkerID = ""
		current.LeaseExpiresAt = time.Time{}
		current.Status = "PENDING"
		if current.Attempt > 0 {
			current.Attempt--
		}
		r.leases[current.TaskID] = current
		released++
	}
	return released, nil
}

func (r *claimLeaseRepo) RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	return r.RefreshClaimedTaskLease(ctx, taskID, workerID, leaseVersion, leaseTTL)
}

func (r *claimLeaseRepo) GetTaskForLease(_ context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return nil, repository.ErrTaskLeaseInvalid
	}
	copy := lease
	return &copy, nil
}

func (r *claimLeaseRepo) GetSubmission(_ context.Context, id int64) (*repository.SubmissionView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	submission, ok := r.submissions[id]
	if !ok {
		return nil, repository.ErrSubmissionNotFound
	}
	copy := *submission
	return &copy, nil
}

func (r *claimLeaseRepo) GetProblemMeta(context.Context, int64) (*repository.ProblemMeta, error) {
	return nil, errors.New("problem metadata should not be read for snapshot artifacts")
}

func (r *claimLeaseRepo) MarkTaskSucceeded(context.Context, string, string, int, repository.TaskSuccessTransition) error {
	return nil
}

func (r *claimLeaseRepo) MarkTaskFailed(context.Context, string, string, int, repository.TaskFailureTransition) (repository.TaskFailureOutcome, error) {
	return repository.TaskFailureOutcome{Status: "SYSTEM_ERROR"}, nil
}
