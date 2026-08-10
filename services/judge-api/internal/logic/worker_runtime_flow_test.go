package logic

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
)

func TestWorkerRuntimeFlowClaimsStreamTaskAndPublishesResult(t *testing.T) {
	ctx := context.Background()
	tmp := t.TempDir()

	sourcePath := filepath.Join(tmp, "main.cpp")
	if err := os.WriteFile(sourcePath, []byte("int main() { return 0; }\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	packageDir := filepath.Join(tmp, "problem-package")
	if err := os.MkdirAll(packageDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(packageDir, "manifest.json"), []byte(`{"cases":[1]}`), 0o644); err != nil {
		t.Fatal(err)
	}

	objects := map[string][]byte{}
	storageServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodPut:
			key := storageTestObjectKey(t, r.URL.Path)
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			objects[key] = body
			sum := sha256.Sum256(body)
			_ = json.NewEncoder(w).Encode(storageObjectMetadata{
				Bucket:      "submissions",
				Key:         key,
				SizeBytes:   int64(len(body)),
				SHA256:      hex.EncodeToString(sum[:]),
				ContentType: r.Header.Get("Content-Type"),
			})
		case http.MethodGet:
			key := storageTestObjectKey(t, r.URL.Path)
			body, ok := objects[key]
			if !ok {
				http.NotFound(w, r)
				return
			}
			sum := sha256.Sum256(body)
			if strings.HasPrefix(r.URL.Path, "/api/storage/metadata/") {
				_ = json.NewEncoder(w).Encode(storageObjectMetadata{
					Bucket:      "submissions",
					Key:         key,
					SizeBytes:   int64(len(body)),
					SHA256:      hex.EncodeToString(sum[:]),
					ContentType: "text/plain; charset=utf-8",
				})
				return
			}
			w.Header().Set("X-OJOS-Object-Sha256", hex.EncodeToString(sum[:]))
			_, _ = w.Write(body)
		default:
			t.Fatalf("unexpected storage request %s %s", r.Method, r.URL.Path)
		}
	}))
	defer storageServer.Close()

	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	problemID := time.Now().UnixNano()
	repo := &fakeWorkerRuntimeRepo{
		submissions: map[int64]*repository.SubmissionView{
			88: {
				ID:         88,
				ProblemID:  8801,
				Language:   "cpp17",
				CodePath:   sourcePath,
				ResultPath: "storage://submissions/88-result.json",
			},
		},
		problems: map[int64]*repository.ProblemMeta{
			8801: {
				ID:         problemID,
				PackageDir: packageDir,
				Status:     "READY",
				Visibility: "PUBLIC",
			},
		},
		leases: map[string]repository.TaskLeaseView{
			"sub-88": {
				TaskID:         "sub-88",
				SubmissionID:   88,
				ProblemID:      8801,
				Language:       "cpp17",
				LeaseVersion:   6,
				LeaseExpiresAt: time.Now().Add(time.Minute),
				Attempt:        1,
				Status:         "PENDING",
			},
		},
	}
	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			Storage:          config.StorageConfig{ServiceEndpoint: storageServer.URL, Bucket: "submissions"},
			WorkerAuth:       config.WorkerAuthConfig{LeaseTTLSeconds: 45},
			WorkloadIdentity: config.WorkloadIdentityConfig{AllowLegacyWorkerToken: true},
		},
		WorkerRepo: repo,
		Redis:      redisClient,
	}

	if err := publishJudgeTaskEvent(ctx, svcCtx, "submission.created", "judge-api-service", 88); err != nil {
		t.Fatalf("publishJudgeTaskEvent returned error: %v", err)
	}
	taskEntries, err := redisClient.XRange(ctx, judgeSubmissionStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read task stream: %v", err)
	}
	if len(taskEntries) != 1 || taskEntries[0].Values["task_id"] != "sub-88" {
		t.Fatalf("task stream must carry deterministic task id, got %#v", taskEntries)
	}

	claimResp, err := NewWorkerClaimTasksLogic(ctx, svcCtx).WorkerClaimTasks(&types.WorkerClaimTasksReq{
		WorkerId:           "worker-a",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     2,
		TaskIds:            []string{" sub-88 ", "", "sub-88", "sub-missing"},
	})
	if err != nil {
		t.Fatalf("WorkerClaimTasks returned error: %v", err)
	}
	if !repo.recoveredStale {
		t.Fatalf("worker claim must recover stale tasks before claiming")
	}
	if want := []string{"sub-88", "sub-missing"}; !reflect.DeepEqual(repo.claimedTaskIDs, want) {
		t.Fatalf("worker claim should normalize Redis task ids, got %#v want %#v", repo.claimedTaskIDs, want)
	}
	if len(claimResp.Tasks) != 1 {
		t.Fatalf("expected one claimed task, got %#v", claimResp.Tasks)
	}
	claimed := claimResp.Tasks[0]
	if claimed.TaskId != "sub-88" || claimed.SubmissionId != 88 || claimed.Source.Sha256 == "" || claimed.ProblemPackage.Sha256 == "" {
		t.Fatalf("unexpected claimed task lease: %#v", claimed)
	}

	submitResp, err := NewWorkerSubmitResultLogic(ctx, svcCtx).WorkerSubmitResult(&types.WorkerSubmitResultReq{
		TaskId:       claimed.TaskId,
		WorkerId:     "worker-a",
		LeaseVersion: claimed.LeaseVersion,
		Status:       "ACCEPTED",
		Score:        100,
		TimeMs:       12,
		MemoryKb:     2048,
		Message:      "accepted",
		Cases: []types.WorkerResultCase{{
			CaseNo:     1,
			Status:     "ACCEPTED",
			Score:      100,
			TimeMs:     12,
			MemoryKb:   2048,
			Stdout:     "ok\n",
			CheckerLog: "matched",
		}},
	})
	if err != nil {
		t.Fatalf("WorkerSubmitResult returned error: %v", err)
	}
	if !submitResp.Accepted || submitResp.Status != "ACCEPTED" {
		t.Fatalf("unexpected submit response: %#v", submitResp)
	}
	if repo.succeededStatus != "ACCEPTED" || repo.succeededTaskID != "sub-88" {
		t.Fatalf("worker result was not marked succeeded in repo: %#v", repo)
	}
	resultPath := repo.submissions[88].ResultPath
	_, resultKey, ok := parseStorageRef(resultPath)
	if !ok || !strings.HasPrefix(resultKey, fmt.Sprintf("judge-results-88-%d-", claimed.LeaseVersion)) {
		t.Fatalf("result path was not promoted to the lease-versioned object: %q", resultPath)
	}
	if _, ok := objects[resultKey]; !ok {
		t.Fatalf("result json %q was not written to storage-service; keys=%#v", resultKey, reflect.ValueOf(objects).MapKeys())
	}
	if _, oldExists := objects["88-result.json"]; oldExists {
		t.Fatal("worker overwrote the fixed legacy result object")
	}
	result, err := readResultFileWithStorage(ctx, svcCtx.Config.Storage, resultPath)
	if err != nil {
		t.Fatalf("readResultFileWithStorage returned error: %v", err)
	}
	stdout, truncated, err := readTruncatedTextWithStorage(ctx, svcCtx.Config.Storage, result.Cases[0].StdoutPath, 1024)
	if err != nil {
		t.Fatalf("readTruncatedTextWithStorage returned error: %v", err)
	}
	if truncated || stdout != "ok\n" {
		t.Fatalf("unexpected stored stdout %q truncated=%v", stdout, truncated)
	}

	resultEntries, err := redisClient.XRange(ctx, judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(resultEntries) != 1 {
		t.Fatalf("expected one result stream entry, got %d", len(resultEntries))
	}
	values := resultEntries[0].Values
	if values["task_id"] != "sub-88" || values["submission_id"] != "88" || values["status"] != "ACCEPTED" {
		t.Fatalf("unexpected result stream values: %#v", values)
	}
}

func TestWorkerSubmitResultRejectsExpiredLeaseBeforeStorageWrite(t *testing.T) {
	var storageRequests atomic.Int32
	storageServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		storageRequests.Add(1)
		http.Error(w, "expired lease must not reach storage", http.StatusInternalServerError)
	}))
	defer storageServer.Close()

	repo := &fakeWorkerRuntimeRepo{
		submissions: map[int64]*repository.SubmissionView{
			88: {
				ID:         88,
				ProblemID:  8801,
				Language:   "cpp17",
				ResultPath: "storage://submissions/existing-result.json",
			},
		},
		leases: map[string]repository.TaskLeaseView{
			"sub-88": {
				TaskID:         "sub-88",
				SubmissionID:   88,
				ProblemID:      8801,
				Language:       "cpp17",
				WorkerID:       "worker-a",
				LeaseVersion:   3,
				LeaseExpiresAt: time.Now().Add(-time.Second),
				Attempt:        1,
				Status:         "RUNNING",
			},
		},
	}
	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			Storage:          config.StorageConfig{ServiceEndpoint: storageServer.URL, Bucket: "submissions"},
			WorkloadIdentity: config.WorkloadIdentityConfig{AllowLegacyWorkerToken: true},
		},
		WorkerRepo: repo,
	}

	resp, err := NewWorkerSubmitResultLogic(context.Background(), svcCtx).WorkerSubmitResult(&types.WorkerSubmitResultReq{
		TaskId:       "sub-88",
		WorkerId:     "worker-a",
		LeaseVersion: 3,
		Status:       "ACCEPTED",
		Score:        100,
		Message:      "late result",
	})
	if err != nil {
		t.Fatalf("WorkerSubmitResult returned error: %v", err)
	}
	if resp.Accepted || resp.Status != "STALE_LEASE" {
		t.Fatalf("expired lease must be rejected, got %#v", resp)
	}
	if got := storageRequests.Load(); got != 0 {
		t.Fatalf("expired lease caused %d storage requests", got)
	}
	if got := repo.submissions[88].ResultPath; got != "storage://submissions/existing-result.json" {
		t.Fatalf("expired lease changed canonical result path to %q", got)
	}
	if repo.succeededSaveCount != 0 {
		t.Fatalf("expired lease reached the success transition")
	}
}

func TestWorkerFailTaskPublishesTerminalFailureResultEvent(t *testing.T) {
	ctx := context.Background()
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	repo := &fakeWorkerRuntimeRepo{
		leases: map[string]repository.TaskLeaseView{
			"sub-91": {
				TaskID:       "sub-91",
				SubmissionID: 91,
				WorkerID:     "worker-a",
				LeaseVersion: 2,
				Status:       "RUNNING",
			},
		},
	}
	svcCtx := &svc.ServiceContext{WorkerRepo: repo, Redis: redisClient}

	resp, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(&types.WorkerFailTaskReq{
		TaskId:       "sub-91",
		WorkerId:     "worker-a",
		LeaseVersion: 2,
		ErrorType:    "SYSTEM",
		Message:      "nsjail failed",
		Retryable:    false,
	})
	if err != nil {
		t.Fatalf("WorkerFailTask returned error: %v", err)
	}
	if !resp.Accepted || resp.Status != "SYSTEM_ERROR" {
		t.Fatalf("unexpected fail response: %#v", resp)
	}
	if repo.failedStatus != "SYSTEM_ERROR" || repo.failedMessage != "nsjail failed" {
		t.Fatalf("failure was not recorded in repo: %#v", repo)
	}

	entries, err := redisClient.XRange(ctx, judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one terminal failure event, got %d", len(entries))
	}
	values := entries[0].Values
	if values["task_id"] != "sub-91" || values["status"] != "SYSTEM_ERROR" || values["message"] != "nsjail failed" {
		t.Fatalf("unexpected failure result stream values: %#v", values)
	}
}

func TestWorkerFailTaskAcceptsExactDuplicateWithoutSecondStateTransition(t *testing.T) {
	ctx := context.Background()
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	repo := &fakeWorkerRuntimeRepo{
		leases: map[string]repository.TaskLeaseView{
			"sub-92": {
				TaskID:       "sub-92",
				SubmissionID: 92,
				WorkerID:     "worker-a",
				LeaseVersion: 4,
				Status:       "RUNNING",
			},
		},
	}
	svcCtx := &svc.ServiceContext{WorkerRepo: repo, Redis: redisClient}
	req := &types.WorkerFailTaskReq{
		TaskId:       "sub-92",
		WorkerId:     "worker-a",
		LeaseVersion: 4,
		ErrorType:    "SYSTEM",
		Message:      "artifact download failed",
		Retryable:    false,
	}

	for attempt := 1; attempt <= 2; attempt++ {
		resp, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(req)
		if err != nil {
			t.Fatalf("duplicate fail attempt %d returned error: %v", attempt, err)
		}
		if !resp.Accepted || resp.Status != "SYSTEM_ERROR" {
			t.Fatalf("unexpected duplicate fail response %d: %#v", attempt, resp)
		}
	}
	if repo.failedSaveCount != 1 {
		t.Fatalf("duplicate fail must save one state transition, got %d", repo.failedSaveCount)
	}
	entries, err := redisClient.XRange(ctx, judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("duplicate fail must publish one terminal event, got %d", len(entries))
	}
}

func TestWorkerTerminalResultReceiptReplaysExactPayloadAndRejectsConflicts(t *testing.T) {
	ctx := context.Background()
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	resultDir := filepath.Join(t.TempDir(), "submission")
	if err := os.MkdirAll(resultDir, 0o755); err != nil {
		t.Fatalf("create result dir: %v", err)
	}
	repo := &fakeWorkerRuntimeRepo{
		leases: map[string]repository.TaskLeaseView{
			"sub-93": {
				TaskID:         "sub-93",
				SubmissionID:   93,
				WorkerID:       "worker-a",
				LeaseVersion:   6,
				LeaseExpiresAt: time.Now().Add(time.Minute),
				Status:         "RUNNING",
			},
		},
		submissions: map[int64]*repository.SubmissionView{
			93: {ID: 93, ResultPath: filepath.Join(resultDir, "result.json")},
		},
	}
	svcCtx := &svc.ServiceContext{WorkerRepo: repo, Redis: redisClient}
	req := &types.WorkerSubmitResultReq{
		TaskId:       "sub-93",
		WorkerId:     "worker-a",
		LeaseVersion: 6,
		Status:       "ACCEPTED",
		Score:        100,
		TimeMs:       12,
		MemoryKb:     2048,
		Message:      "accepted",
	}

	for attempt := 1; attempt <= 2; attempt++ {
		resp, err := NewWorkerSubmitResultLogic(ctx, svcCtx).WorkerSubmitResult(req)
		if err != nil {
			t.Fatalf("result attempt %d returned error: %v", attempt, err)
		}
		if !resp.Accepted || resp.Status != "ACCEPTED" {
			t.Fatalf("unexpected result receipt replay %d: %#v", attempt, resp)
		}
	}
	if repo.succeededSaveCount != 1 {
		t.Fatalf("exact result retry must save once, got %d", repo.succeededSaveCount)
	}
	entries, err := redisClient.XRange(ctx, judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("exact result retry must publish one fixture event, got %d", len(entries))
	}

	conflicting := *req
	conflicting.Score = 99
	resp, err := NewWorkerSubmitResultLogic(ctx, svcCtx).WorkerSubmitResult(&conflicting)
	if err != nil {
		t.Fatalf("conflicting result returned error: %v", err)
	}
	if resp.Accepted || resp.Status != "STALE_LEASE" {
		t.Fatalf("conflicting same-lease result must fail closed: %#v", resp)
	}

	failResp, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(&types.WorkerFailTaskReq{
		TaskId:       "sub-93",
		WorkerId:     "worker-a",
		LeaseVersion: 6,
		ErrorType:    "SYSTEM",
		Message:      "conflicting report kind",
		Retryable:    false,
	})
	if err != nil {
		t.Fatalf("conflicting fail report returned error: %v", err)
	}
	if failResp.Accepted || failResp.Status != "STALE_LEASE" {
		t.Fatalf("result/fail conflict on one lease must fail closed: %#v", failResp)
	}
}

func TestRetryableFailReceiptReplaysAfterTaskHasNextLease(t *testing.T) {
	ctx := context.Background()
	repo := &fakeWorkerRuntimeRepo{
		leases: map[string]repository.TaskLeaseView{
			"sub-94": {
				TaskID:       "sub-94",
				SubmissionID: 94,
				WorkerID:     "worker-a",
				LeaseVersion: 1,
				Status:       "RUNNING",
			},
		},
	}
	svcCtx := &svc.ServiceContext{WorkerRepo: repo}
	req := &types.WorkerFailTaskReq{
		TaskId:       "sub-94",
		WorkerId:     "worker-a",
		LeaseVersion: 1,
		ErrorType:    "SYSTEM",
		Message:      "temporary download failure",
		Retryable:    true,
	}
	first, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(req)
	if err != nil || !first.Accepted || first.Status != "PENDING" {
		t.Fatalf("first retryable fail: resp=%#v err=%v", first, err)
	}
	lease := repo.leases["sub-94"]
	lease.WorkerID = "worker-b"
	lease.LeaseVersion = 2
	lease.Status = "RUNNING"
	repo.leases["sub-94"] = lease

	replayed, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(req)
	if err != nil || !replayed.Accepted || replayed.Status != "PENDING" {
		t.Fatalf("old lease receipt replay: resp=%#v err=%v", replayed, err)
	}
	conflict := *req
	conflict.Message = "different retryable failure"
	conflicting, err := NewWorkerFailTaskLogic(ctx, svcCtx).WorkerFailTask(&conflict)
	if err != nil {
		t.Fatalf("conflicting retryable fail returned error: %v", err)
	}
	if conflicting.Accepted || conflicting.Status != "STALE_LEASE" {
		t.Fatalf("conflicting old lease report must fail closed: %#v", conflicting)
	}
	if repo.failedSaveCount != 1 {
		t.Fatalf("retryable fail receipt must save once, got %d", repo.failedSaveCount)
	}
}

type fakeWorkerRuntimeRepo struct {
	workers     map[string]repository.WorkerView
	submissions map[int64]*repository.SubmissionView
	problems    map[int64]*repository.ProblemMeta
	leases      map[string]repository.TaskLeaseView

	recoveredStale bool
	claimedTaskIDs []string

	succeededTaskID    string
	succeededStatus    string
	succeededSaveCount int
	failedStatus       string
	failedMessage      string
	failedSaveCount    int
	reportReceipts     map[fakeReportReceiptKey]fakeReportReceipt
}

type fakeReportReceiptKey struct {
	taskID       string
	leaseVersion int
}

type fakeReportReceipt struct {
	kind          string
	payloadSHA256 string
}

func (r *fakeWorkerRuntimeRepo) UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error) {
	if r.workers == nil {
		r.workers = map[string]repository.WorkerView{}
	}
	view := repository.WorkerView{
		WorkerID:           w.WorkerID,
		WorkerName:         w.WorkerName,
		Hostname:           w.Hostname,
		Version:            w.Version,
		Capabilities:       append([]string(nil), w.Capabilities...),
		SupportedLanguages: append([]string(nil), w.SupportedLanguages...),
		MaxConcurrency:     w.MaxConcurrency,
		Status:             "ONLINE",
		LastSeen:           time.Now(),
		RegisteredAt:       time.Now(),
		UpdatedAt:          time.Now(),
	}
	r.workers[w.WorkerID] = view
	return &view, nil
}

func (r *fakeWorkerRuntimeRepo) WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error) {
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

func (r *fakeWorkerRuntimeRepo) RecoverStaleTasks(ctx context.Context) (int64, error) {
	r.recoveredStale = true
	return 0, nil
}

func (r *fakeWorkerRuntimeRepo) ClaimTasks(
	ctx context.Context,
	workerID string,
	supportedLanguages []string,
	limit int,
	leaseTTL time.Duration,
	taskIDs []string,
) ([]repository.TaskLeaseView, error) {
	r.claimedTaskIDs = append([]string(nil), taskIDs...)
	if workerID == "" {
		return nil, repository.ErrWorkerNotFound
	}
	if limit <= 0 {
		return []repository.TaskLeaseView{}, nil
	}

	claimed := make([]repository.TaskLeaseView, 0)
	for _, taskID := range taskIDs {
		lease, ok := r.leases[taskID]
		if !ok || lease.Status != "PENDING" || !fakeContainsString(supportedLanguages, lease.Language) {
			continue
		}
		lease.WorkerID = workerID
		lease.LeaseVersion++
		lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
		lease.Status = "RUNNING"
		r.leases[taskID] = lease
		claimed = append(claimed, lease)
		if len(claimed) >= limit {
			break
		}
	}
	return claimed, nil
}

func (r *fakeWorkerRuntimeRepo) RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return nil, repository.ErrTaskLeaseInvalid
	}
	lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
	lease.HeartbeatAt = time.Now()
	r.leases[taskID] = lease
	return &lease, nil
}

func (r *fakeWorkerRuntimeRepo) RefreshClaimedTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return nil, repository.ErrTaskLeaseInvalid
	}
	lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
	lease.HeartbeatAt = time.Now()
	r.leases[taskID] = lease
	return &lease, nil
}

func (r *fakeWorkerRuntimeRepo) ReleaseClaimedTasks(ctx context.Context, workerID string, leases []repository.TaskLeaseView, reason string) (int64, error) {
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

func (r *fakeWorkerRuntimeRepo) GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error) {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return nil, repository.ErrTaskLeaseInvalid
	}
	return &lease, nil
}

func (r *fakeWorkerRuntimeRepo) GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error) {
	submission, ok := r.submissions[id]
	if !ok {
		return nil, errors.New("submission not found")
	}
	return submission, nil
}

func (r *fakeWorkerRuntimeRepo) GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error) {
	problem, ok := r.problems[id]
	if !ok {
		return nil, errors.New("problem not found")
	}
	return problem, nil
}

func (r *fakeWorkerRuntimeRepo) MarkTaskSucceeded(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	transition repository.TaskSuccessTransition,
) error {
	duplicate, err := r.saveReportReceipt(taskID, leaseVersion, "result", transition.PayloadSHA256)
	if err != nil {
		return err
	}
	if duplicate {
		return repository.ErrTaskTransitionAlreadySaved
	}
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return repository.ErrTaskLeaseInvalid
	}
	if lease.Status != "RUNNING" {
		return repository.ErrTaskLeaseInvalid
	}
	lease.Status = "SUCCEEDED"
	r.leases[taskID] = lease
	if submission, ok := r.submissions[lease.SubmissionID]; ok && transition.ResultPath != "" {
		submission.ResultPath = transition.ResultPath
	}
	r.succeededTaskID = taskID
	r.succeededStatus = transition.Status
	r.succeededSaveCount++
	return nil
}

func (r *fakeWorkerRuntimeRepo) MarkTaskFailed(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	transition repository.TaskFailureTransition,
) (repository.TaskFailureOutcome, error) {
	outcome := repository.TaskFailureOutcome{Status: transition.Status}
	if transition.Retryable {
		outcome.Status = "PENDING"
		outcome.RetryScheduled = true
	}
	duplicate, err := r.saveReportReceipt(taskID, leaseVersion, "fail", transition.PayloadSHA256)
	if err != nil {
		return repository.TaskFailureOutcome{}, err
	}
	if duplicate {
		outcome.AlreadySaved = true
		return outcome, repository.ErrTaskTransitionAlreadySaved
	}
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return repository.TaskFailureOutcome{}, repository.ErrTaskLeaseInvalid
	}
	if transition.Retryable {
		lease.Status = "PENDING"
	} else {
		lease.Status = "FAILED"
	}
	r.leases[taskID] = lease
	r.failedStatus = transition.Status
	r.failedMessage = transition.Message
	r.failedSaveCount++
	return outcome, nil
}

func (r *fakeWorkerRuntimeRepo) saveReportReceipt(
	taskID string,
	leaseVersion int,
	kind string,
	payloadSHA256 string,
) (bool, error) {
	if r.reportReceipts == nil {
		r.reportReceipts = make(map[fakeReportReceiptKey]fakeReportReceipt)
	}
	key := fakeReportReceiptKey{taskID: taskID, leaseVersion: leaseVersion}
	if saved, ok := r.reportReceipts[key]; ok {
		if saved.kind == kind && saved.payloadSHA256 == payloadSHA256 {
			return true, nil
		}
		return false, repository.ErrTaskLeaseInvalid
	}
	r.reportReceipts[key] = fakeReportReceipt{kind: kind, payloadSHA256: payloadSHA256}
	return false, nil
}

func fakeContainsString(items []string, value string) bool {
	if len(items) == 0 {
		return true
	}
	for _, item := range items {
		if item == value {
			return true
		}
	}
	return false
}
