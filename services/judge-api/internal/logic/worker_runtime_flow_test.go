package logic

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
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
			Storage:    config.StorageConfig{ServiceEndpoint: storageServer.URL, Bucket: "submissions"},
			WorkerAuth: config.WorkerAuthConfig{LeaseTTLSeconds: 45},
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
	if _, ok := objects["88-result.json"]; !ok {
		t.Fatalf("result json was not written to storage-service")
	}
	result, err := readResultFileWithStorage(ctx, svcCtx.Config.Storage, "storage://submissions/88-result.json")
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

type fakeWorkerRuntimeRepo struct {
	workers     map[string]repository.WorkerView
	submissions map[int64]*repository.SubmissionView
	problems    map[int64]*repository.ProblemMeta
	leases      map[string]repository.TaskLeaseView

	recoveredStale bool
	claimedTaskIDs []string

	succeededTaskID string
	succeededStatus string
	failedStatus    string
	failedMessage   string
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
	status string,
	score int,
	timeMS int,
	memoryKB int,
	message string,
) error {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return repository.ErrTaskLeaseInvalid
	}
	lease.Status = "SUCCEEDED"
	r.leases[taskID] = lease
	r.succeededTaskID = taskID
	r.succeededStatus = status
	return nil
}

func (r *fakeWorkerRuntimeRepo) MarkTaskFailed(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	status string,
	message string,
	retryable bool,
) error {
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
	r.failedStatus = status
	r.failedMessage = message
	return nil
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
