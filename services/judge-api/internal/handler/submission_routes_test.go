package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
)

func TestCreateSubmissionRouteStoresSourceAndPublishesTask(t *testing.T) {
	ctx := context.Background()
	storageEndpoint, objects, stopStorage := startJudgeWorkerStorageServer(t)
	defer stopStorage()

	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	repo := &fakeSubmissionHTTPRepo{
		nextSubmissionID: 42,
		problems: map[int64]*repository.ProblemMeta{
			1001: {
				ID:         1001,
				PackageDir: filepath.ToSlash(filepath.Join(t.TempDir(), "problem-package")),
				Status:     "ready",
				Visibility: "public",
			},
		},
	}
	permissions := &fakePermissionChecker{
		allowed: map[string]bool{
			"7:judge.submit:system:0": true,
		},
	}
	endpoint, stop := startJudgeSubmissionHTTPServer(t, repo, permissions, redisClient, storageEndpoint)
	defer stop()

	unauthorized := postUserJSON(t, endpoint+"/judge/submissions", 0, map[string]any{
		"problem_id": 1001,
		"language":   "cpp17",
		"code":       "int main() { return 0; }\n",
	})
	defer unauthorized.Body.Close()
	if unauthorized.StatusCode != http.StatusUnauthorized {
		body, _ := io.ReadAll(unauthorized.Body)
		t.Fatalf("submission route without user context should be unauthorized, got %d: %s", unauthorized.StatusCode, string(body))
	}

	resp := postUserJSON(t, endpoint+"/judge/submissions", 7, map[string]any{
		"problem_id": 1001,
		"language":   "cpp17",
		"code":       "int main() { return 0; }\n",
	})
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		t.Fatalf("create submission status %d: %s", resp.StatusCode, string(body))
	}
	var body types.CreateSubmissionResp
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("decode create submission response: %v", err)
	}
	if body.SubmissionId != 42 || body.Status != "PENDING" {
		t.Fatalf("unexpected create submission response: %#v", body)
	}

	if repo.createdProblemID != 1001 || repo.createdUserID != 7 || repo.createdLanguage != "cpp17" {
		t.Fatalf("submission repo did not receive configured language create call: %#v", repo)
	}
	if repo.updatedSubmissionID != 42 || repo.updatedCodePath != "storage://submissions/42-source-main.cpp" || repo.updatedResultPath != "storage://submissions/42-result.json" {
		t.Fatalf("submission source paths were not storage-service refs: %#v", repo)
	}
	if repo.updatedCodeSha256 == "" {
		t.Fatalf("submission source sha256 must be recorded")
	}
	if !repo.taskEnsured || repo.ensuredSubmissionID != 42 {
		t.Fatalf("submission task was not ensured: %#v", repo)
	}
	if _, ok := objects.get("42-source-main.cpp"); !ok {
		t.Fatalf("submission source was not stored in storage-service")
	}

	entries, err := redisClient.XRange(ctx, "ojos:judge:task", "-", "+").Result()
	if err != nil {
		t.Fatalf("read judge submission stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one judge task stream entry, got %d", len(entries))
	}
	values := entries[0].Values
	if values["type"] != "submission.created" || values["task_id"] != "sub-42" || values["submission_id"] != "42" {
		t.Fatalf("unexpected judge task stream values: %#v", values)
	}
	groups, err := redisClient.XInfoGroups(ctx, "ojos:judge:task").Result()
	if err != nil {
		t.Fatalf("read judge task consumer group: %v", err)
	}
	if len(groups) != 1 || groups[0].Name != "judge-worker" {
		t.Fatalf("expected judge-worker consumer group, got %#v", groups)
	}
	if got, want := permissions.calls, []string{"7:judge.submit:system:0"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("unexpected permission calls: got %#v want %#v", got, want)
	}
}

func TestJudgeSubmissionWorkerHTTPRuntimeLoop(t *testing.T) {
	ctx := context.Background()
	storageEndpoint, objects, stopStorage := startJudgeInternalGatewayStorageServer(t)
	defer stopStorage()

	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	_, packageDir := writeWorkerArtifacts(t)
	repo := &fakeJudgeRuntimeHTTPRepo{
		nextSubmissionID: 77,
		workers:          map[string]repository.WorkerView{},
		submissions:      map[int64]*repository.SubmissionView{},
		leases:           map[string]repository.TaskLeaseView{},
		problems: map[int64]*repository.ProblemMeta{
			2001: {
				ID:         2001,
				PackageDir: packageDir,
				Status:     "ready",
				Visibility: "public",
			},
		},
	}
	permissions := &fakePermissionChecker{
		allowed: map[string]bool{
			"7:judge.submit:system:0": true,
		},
	}
	endpoint, stop := startJudgeRuntimeHTTPServer(t, repo, permissions, redisClient, storageEndpoint, "secret-worker-token")
	defer stop()

	register := postJSON(t, endpoint+"/judge/worker/register", "secret-worker-token", map[string]any{
		"worker_id":           "worker-a",
		"worker_name":         "worker a",
		"hostname":            "judge-node-1",
		"version":             "test",
		"supported_languages": []string{"cpp17"},
		"max_concurrency":     1,
	})
	defer register.Body.Close()
	if register.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(register.Body)
		t.Fatalf("register worker status %d: %s", register.StatusCode, string(body))
	}

	create := postUserJSON(t, endpoint+"/judge/submissions", 7, map[string]any{
		"problem_id": 2001,
		"language":   "cpp17",
		"code":       "int main() { return 0; }\n",
	})
	defer create.Body.Close()
	if create.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(create.Body)
		t.Fatalf("create submission status %d: %s", create.StatusCode, string(body))
	}
	var createResp types.CreateSubmissionResp
	if err := json.NewDecoder(create.Body).Decode(&createResp); err != nil {
		t.Fatalf("decode create submission response: %v", err)
	}
	if createResp.SubmissionId != 77 || createResp.Status != "PENDING" {
		t.Fatalf("unexpected create response: %#v", createResp)
	}

	streams, err := redisClient.XReadGroup(ctx, &redis.XReadGroupArgs{
		Group:    "judge-worker",
		Consumer: "worker-a",
		Streams:  []string{"ojos:judge:task", ">"},
		Count:    1,
		Block:    time.Second,
	}).Result()
	if err != nil {
		t.Fatalf("worker must be able to read submission task from Redis Stream: %v", err)
	}
	if len(streams) != 1 || len(streams[0].Messages) != 1 {
		t.Fatalf("expected one Redis task event, got %#v", streams)
	}
	taskID, _ := streams[0].Messages[0].Values["task_id"].(string)
	if taskID != "sub-77" {
		t.Fatalf("unexpected stream task id %q from %#v", taskID, streams[0].Messages[0].Values)
	}

	claim := postJSON(t, endpoint+"/judge/worker/tasks/claim", "secret-worker-token", map[string]any{
		"worker_id":           "worker-a",
		"supported_languages": []string{"cpp17"},
		"available_slots":     1,
		"task_ids":            []string{taskID},
	})
	defer claim.Body.Close()
	if claim.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(claim.Body)
		t.Fatalf("claim task status %d: %s", claim.StatusCode, string(body))
	}
	var claimResp types.WorkerClaimTasksResp
	if err := json.NewDecoder(claim.Body).Decode(&claimResp); err != nil {
		t.Fatalf("decode claim response: %v", err)
	}
	if len(claimResp.Tasks) != 1 {
		t.Fatalf("expected one claimed task, got %#v", claimResp.Tasks)
	}
	claimed := claimResp.Tasks[0]
	if claimed.TaskId != "sub-77" || claimed.SubmissionId != 77 || claimed.Source.Sha256 == "" || claimed.ProblemPackage.Sha256 == "" {
		t.Fatalf("unexpected claimed task: %#v", claimed)
	}

	if !strings.HasPrefix(claimed.Source.Url, "/internal/apis/storage.object.get/") {
		t.Fatalf("worker source should use internal gateway resolver url, got %q", claimed.Source.Url)
	}
	source := getWithServiceIdentity(t, storageEndpoint+claimed.Source.Url, "judge-worker", "child-node", "internal-token")
	defer source.Body.Close()
	sourceBody, err := io.ReadAll(source.Body)
	if err != nil {
		t.Fatalf("read source artifact: %v", err)
	}
	if source.StatusCode != http.StatusOK || string(sourceBody) != "int main() { return 0; }\n" {
		t.Fatalf("unexpected source artifact status=%d body=%q", source.StatusCode, string(sourceBody))
	}

	packageResp := getWithWorkerToken(t, endpoint+claimed.ProblemPackage.Url, "secret-worker-token")
	defer packageResp.Body.Close()
	packageBody, err := io.ReadAll(packageResp.Body)
	if err != nil {
		t.Fatalf("read problem package artifact: %v", err)
	}
	if packageResp.StatusCode != http.StatusOK || len(packageBody) == 0 {
		t.Fatalf("unexpected problem package status=%d bytes=%d", packageResp.StatusCode, len(packageBody))
	}

	result := postJSON(t, endpoint+"/judge/worker/tasks/sub-77/result", "secret-worker-token", map[string]any{
		"worker_id":     "worker-a",
		"lease_version": claimed.LeaseVersion,
		"status":        "ACCEPTED",
		"score":         100,
		"time_ms":       11,
		"memory_kb":     2048,
		"message":       "accepted",
		"cases": []map[string]any{{
			"case_no":     1,
			"status":      "ACCEPTED",
			"score":       100,
			"time_ms":     11,
			"memory_kb":   2048,
			"stdout":      "ok\n",
			"checker_log": "matched",
		}},
	})
	defer result.Body.Close()
	if result.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(result.Body)
		t.Fatalf("submit result status %d: %s", result.StatusCode, string(body))
	}
	var resultResp types.WorkerSubmitResultResp
	if err := json.NewDecoder(result.Body).Decode(&resultResp); err != nil {
		t.Fatalf("decode result response: %v", err)
	}
	if !resultResp.Accepted || resultResp.Status != "ACCEPTED" {
		t.Fatalf("unexpected result response: %#v", resultResp)
	}
	if repo.succeededTaskID != "sub-77" || repo.succeededStatus != "ACCEPTED" {
		t.Fatalf("worker result was not persisted through repo: %#v", repo)
	}
	if _, ok := objects.get("77-source-main.cpp"); !ok {
		t.Fatalf("submission source object was not stored through internal gateway")
	}
	if _, ok := objects.get("77-result.json"); !ok {
		t.Fatalf("worker result object was not stored")
	}
	resultEntries, err := redisClient.XRange(ctx, "ojos:judge:result", "-", "+").Result()
	if err != nil {
		t.Fatalf("read judge result stream: %v", err)
	}
	if len(resultEntries) != 1 || resultEntries[0].Values["task_id"] != "sub-77" || resultEntries[0].Values["status"] != "ACCEPTED" {
		t.Fatalf("unexpected judge result stream entries: %#v", resultEntries)
	}
}

func startJudgeSubmissionHTTPServer(
	t *testing.T,
	repo svc.SubmissionRepository,
	permissions svc.PermissionChecker,
	redisClient *redis.Client,
	storageEndpoint string,
) (string, func()) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1",
		Port: port,
	})
	if err != nil {
		_ = listener.Close()
		t.Fatalf("new rest server: %v", err)
	}
	_ = listener.Close()

	noOp := func(next http.HandlerFunc) http.HandlerFunc {
		return next
	}
	RegisterHandlers(server, &svc.ServiceContext{
		Config: config.Config{
			Storage: config.StorageConfig{
				SubmissionsRoot: t.TempDir(),
				ServiceEndpoint: storageEndpoint,
				Bucket:          "submissions",
			},
			Languages: judgeRouteTestLanguages(),
		},
		SubmissionRepo:         repo,
		Permission:             permissions,
		Redis:                  redisClient,
		UserContextMiddleware:  middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: noOp,
		WorkerAuthMiddleware:   noOp,
	})
	go server.Start()
	endpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForJudgeWorkerHTTPServer(t, endpoint)
	return endpoint, server.Stop
}

func startJudgeRuntimeHTTPServer(
	t *testing.T,
	repo *fakeJudgeRuntimeHTTPRepo,
	permissions svc.PermissionChecker,
	redisClient *redis.Client,
	storageEndpoint string,
	workerToken string,
) (string, func()) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1",
		Port: port,
	})
	if err != nil {
		_ = listener.Close()
		t.Fatalf("new rest server: %v", err)
	}
	_ = listener.Close()

	noOp := func(next http.HandlerFunc) http.HandlerFunc {
		return next
	}
	RegisterHandlers(server, &svc.ServiceContext{
		Config: config.Config{
			Storage: config.StorageConfig{
				SubmissionsRoot:         t.TempDir(),
				InternalGatewayEndpoint: storageEndpoint,
				Bucket:                  "submissions",
				CallerService:           "judge-api",
				CallerNodeID:            "child-node",
				ServiceToken:            "internal-token",
			},
			WorkerAuth: config.WorkerAuthConfig{Token: workerToken, LeaseTTLSeconds: 45},
			Languages:  judgeRouteTestLanguages(),
		},
		SubmissionRepo:         repo,
		WorkerRepo:             repo,
		Permission:             permissions,
		Redis:                  redisClient,
		UserContextMiddleware:  middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: noOp,
		WorkerAuthMiddleware:   middleware.NewWorkerAuthMiddleware(workerToken).Handle,
	})
	go server.Start()
	endpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForJudgeWorkerHTTPServer(t, endpoint)
	return endpoint, server.Stop
}

func judgeRouteTestLanguages() config.LanguagesConfig {
	return config.LanguagesConfig{Items: []config.LanguageConfig{
		{Id: "cpp17", DisplayName: "C++17", Version: "test", Enabled: true, SourceFile: "main.cpp"},
	}}
}

func postUserJSON(t *testing.T, url string, userID int64, body any) *http.Response {
	t.Helper()
	data, err := json.Marshal(body)
	if err != nil {
		t.Fatalf("marshal request: %v", err)
	}
	req, err := http.NewRequest(http.MethodPost, url, bytes.NewReader(data))
	if err != nil {
		t.Fatalf("build request: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")
	if userID > 0 {
		req.Header.Set(authctx.HeaderAuthVerified, "true")
		req.Header.Set(authctx.HeaderUserID, fmt.Sprintf("%d", userID))
		req.Header.Set(authctx.HeaderUsername, "submitter")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("post %s: %v", url, err)
	}
	return resp
}

type fakeSubmissionHTTPRepo struct {
	mu               sync.Mutex
	nextSubmissionID int64
	problems         map[int64]*repository.ProblemMeta

	createdProblemID int64
	createdUserID    int64
	createdLanguage  string

	updatedSubmissionID int64
	updatedCodePath     string
	updatedCodeSha256   string
	updatedResultPath   string

	taskEnsured         bool
	ensuredSubmissionID int64
	systemErrors        []string
}

func (r *fakeSubmissionHTTPRepo) GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	problem, ok := r.problems[id]
	if !ok {
		return nil, errors.New("problem not found")
	}
	copied := *problem
	return &copied, nil
}

func (r *fakeSubmissionHTTPRepo) CreateSubmission(ctx context.Context, problemID int64, userID int64, language string) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.createdProblemID = problemID
	r.createdUserID = userID
	r.createdLanguage = language
	if r.nextSubmissionID <= 0 {
		r.nextSubmissionID = 1
	}
	return r.nextSubmissionID, nil
}

func (r *fakeSubmissionHTTPRepo) UpdateSubmissionSource(ctx context.Context, submissionID int64, codePath string, codeSha256 string, resultPath string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.updatedSubmissionID = submissionID
	r.updatedCodePath = codePath
	r.updatedCodeSha256 = codeSha256
	r.updatedResultPath = resultPath
	return nil
}

func (r *fakeSubmissionHTTPRepo) EnsureTaskForSubmission(ctx context.Context, submissionID int64) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.taskEnsured = true
	r.ensuredSubmissionID = submissionID
	return nil
}

func (r *fakeSubmissionHTTPRepo) MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.systemErrors = append(r.systemErrors, message)
	return nil
}

type fakePermissionChecker struct {
	mu      sync.Mutex
	allowed map[string]bool
	calls   []string
}

func (p *fakePermissionChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	key := permissionKey(userID, permissionCode, scope)
	p.mu.Lock()
	defer p.mu.Unlock()
	p.calls = append(p.calls, key)
	if p.allowed[key] {
		return nil
	}
	return sharedperm.ErrForbidden
}

func (p *fakePermissionChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) (bool, error) {
	key := permissionKey(userID, permissionCode, scope)
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.allowed[key], nil
}

func permissionKey(userID int64, permissionCode string, scope sharedperm.Scope) string {
	return fmt.Sprintf("%d:%s:%s:%d", userID, permissionCode, scope.Type, scope.ID)
}

type fakeJudgeRuntimeHTTPRepo struct {
	mu               sync.Mutex
	nextSubmissionID int64
	problems         map[int64]*repository.ProblemMeta
	submissions      map[int64]*repository.SubmissionView
	leases           map[string]repository.TaskLeaseView
	workers          map[string]repository.WorkerView

	succeededTaskID string
	succeededStatus string
	failedStatus    string
	failedMessage   string
}

func (r *fakeJudgeRuntimeHTTPRepo) GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	problem, ok := r.problems[id]
	if !ok {
		return nil, errors.New("problem not found")
	}
	copied := *problem
	return &copied, nil
}

func (r *fakeJudgeRuntimeHTTPRepo) CreateSubmission(ctx context.Context, problemID int64, userID int64, language string) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.nextSubmissionID <= 0 {
		r.nextSubmissionID = 1
	}
	id := r.nextSubmissionID
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

func (r *fakeJudgeRuntimeHTTPRepo) UpdateSubmissionSource(ctx context.Context, submissionID int64, codePath string, codeSha256 string, resultPath string) error {
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

func (r *fakeJudgeRuntimeHTTPRepo) EnsureTaskForSubmission(ctx context.Context, submissionID int64) error {
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

func (r *fakeJudgeRuntimeHTTPRepo) MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if submission, ok := r.submissions[submissionID]; ok {
		submission.Status = "SYSTEM_ERROR"
		submission.Message = message
		submission.UpdatedAt = time.Now()
	}
	return nil
}

func (r *fakeJudgeRuntimeHTTPRepo) UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
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

func (r *fakeJudgeRuntimeHTTPRepo) WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error) {
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

func (r *fakeJudgeRuntimeHTTPRepo) RecoverStaleTasks(ctx context.Context) (int64, error) {
	return 0, nil
}

func (r *fakeJudgeRuntimeHTTPRepo) ClaimTasks(
	ctx context.Context,
	workerID string,
	supportedLanguages []string,
	limit int,
	leaseTTL time.Duration,
	taskIDs []string,
) ([]repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, ok := r.workers[workerID]; !ok {
		return nil, repository.ErrWorkerNotFound
	}
	claimed := make([]repository.TaskLeaseView, 0)
	for _, taskID := range taskIDs {
		lease, ok := r.leases[taskID]
		if !ok || lease.Status != "PENDING" || !containsString(supportedLanguages, lease.Language) {
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

func (r *fakeJudgeRuntimeHTTPRepo) RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
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

func (r *fakeJudgeRuntimeHTTPRepo) GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return nil, repository.ErrTaskLeaseInvalid
	}
	return &lease, nil
}

func (r *fakeJudgeRuntimeHTTPRepo) GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	submission, ok := r.submissions[id]
	if !ok {
		return nil, repository.ErrSubmissionNotFound
	}
	copied := *submission
	return &copied, nil
}

func (r *fakeJudgeRuntimeHTTPRepo) MarkTaskSucceeded(
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
	r.mu.Lock()
	defer r.mu.Unlock()
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return repository.ErrTaskLeaseInvalid
	}
	lease.Status = "SUCCEEDED"
	r.leases[taskID] = lease
	if submission, ok := r.submissions[lease.SubmissionID]; ok {
		submission.Status = status
		submission.Score = score
		submission.TimeMS = timeMS
		submission.MemoryKB = memoryKB
		submission.Message = message
		submission.UpdatedAt = time.Now()
	}
	r.succeededTaskID = taskID
	r.succeededStatus = status
	return nil
}

func (r *fakeJudgeRuntimeHTTPRepo) MarkTaskFailed(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	status string,
	message string,
	retryable bool,
) error {
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
	r.failedStatus = status
	r.failedMessage = message
	return nil
}
