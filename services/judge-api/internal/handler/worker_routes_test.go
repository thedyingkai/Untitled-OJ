package handler

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest"
)

func TestWorkerRoutesRequireTokenAndRunClaimResultFlow(t *testing.T) {
	ctx := context.Background()
	storageEndpoint, objects, stopStorage := startJudgeWorkerStorageServer(t)
	defer stopStorage()

	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	sourcePath, packageDir := writeWorkerArtifacts(t)
	repo := &fakeWorkerHTTPRepo{
		workers: map[string]repository.WorkerView{},
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
				ID:         time.Now().UnixNano(),
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
				LeaseVersion:   2,
				LeaseExpiresAt: time.Now().Add(time.Minute),
				Attempt:        1,
				Status:         "PENDING",
			},
		},
	}
	endpoint, stop := startJudgeWorkerHTTPServer(t, repo, redisClient, storageEndpoint, "secret-worker-token")
	defer stop()

	unauthorized := postJSON(t, endpoint+"/judge/worker/tasks/claim", "", map[string]any{
		"worker_id":       "worker-a",
		"available_slots": 1,
		"task_ids":        []string{"sub-88"},
	})
	defer unauthorized.Body.Close()
	if unauthorized.StatusCode != http.StatusUnauthorized {
		body, _ := io.ReadAll(unauthorized.Body)
		t.Fatalf("worker route without token should be unauthorized, got %d: %s", unauthorized.StatusCode, string(body))
	}

	register := postJSON(t, endpoint+"/judge/worker/register", "secret-worker-token", map[string]any{
		"worker_id":           "worker-a",
		"worker_name":         "worker a",
		"hostname":            "judge-node-1",
		"version":             "test",
		"supported_languages": []string{"cpp17"},
		"max_concurrency":     2,
	})
	defer register.Body.Close()
	if register.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(register.Body)
		t.Fatalf("register status %d: %s", register.StatusCode, string(body))
	}
	var registerResp types.WorkerRegisterResp
	if err := json.NewDecoder(register.Body).Decode(&registerResp); err != nil {
		t.Fatalf("decode register response: %v", err)
	}
	if registerResp.WorkerId != "worker-a" || registerResp.Status != "ONLINE" || registerResp.LeaseTtlSeconds != 45 {
		t.Fatalf("unexpected register response: %#v", registerResp)
	}

	heartbeat := postJSON(t, endpoint+"/judge/worker/heartbeat", "secret-worker-token", map[string]any{
		"worker_id":       "worker-a",
		"running_count":   0,
		"running_tasks":   []string{},
		"available_slots": 2,
	})
	defer heartbeat.Body.Close()
	if heartbeat.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(heartbeat.Body)
		t.Fatalf("heartbeat status %d: %s", heartbeat.StatusCode, string(body))
	}
	var heartbeatResp types.WorkerHeartbeatResp
	if err := json.NewDecoder(heartbeat.Body).Decode(&heartbeatResp); err != nil {
		t.Fatalf("decode heartbeat response: %v", err)
	}
	if heartbeatResp.WorkerId != "worker-a" || heartbeatResp.Status != "ONLINE" {
		t.Fatalf("unexpected heartbeat response: %#v", heartbeatResp)
	}

	claim := postJSON(t, endpoint+"/judge/worker/tasks/claim", "secret-worker-token", map[string]any{
		"worker_id":           "worker-a",
		"supported_languages": []string{"cpp17"},
		"available_slots":     2,
		"task_ids":            []string{" sub-88 ", "", "sub-88", "sub-missing"},
	})
	defer claim.Body.Close()
	if claim.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(claim.Body)
		t.Fatalf("claim status %d: %s", claim.StatusCode, string(body))
	}
	var claimResp types.WorkerClaimTasksResp
	if err := json.NewDecoder(claim.Body).Decode(&claimResp); err != nil {
		t.Fatalf("decode claim response: %v", err)
	}
	if len(claimResp.Tasks) != 1 {
		t.Fatalf("expected one claimed task, got %#v", claimResp.Tasks)
	}
	if want := []string{"sub-88", "sub-missing"}; !stringSlicesEqual(repo.claimedTaskIDs, want) {
		t.Fatalf("claim should normalize stream task ids, got %#v want %#v", repo.claimedTaskIDs, want)
	}
	claimed := claimResp.Tasks[0]
	if claimed.TaskId != "sub-88" || claimed.LeaseVersion != 3 || claimed.Source.Sha256 == "" || claimed.ProblemPackage.Sha256 == "" {
		t.Fatalf("unexpected claimed lease: %#v", claimed)
	}

	taskHeartbeat := postJSON(t, endpoint+"/judge/worker/tasks/sub-88/heartbeat", "secret-worker-token", map[string]any{
		"worker_id":     "worker-a",
		"lease_version": claimed.LeaseVersion,
	})
	defer taskHeartbeat.Body.Close()
	if taskHeartbeat.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(taskHeartbeat.Body)
		t.Fatalf("task heartbeat status %d: %s", taskHeartbeat.StatusCode, string(body))
	}
	var taskHeartbeatResp types.WorkerTaskHeartbeatResp
	if err := json.NewDecoder(taskHeartbeat.Body).Decode(&taskHeartbeatResp); err != nil {
		t.Fatalf("decode task heartbeat response: %v", err)
	}
	if taskHeartbeatResp.TaskId != "sub-88" || taskHeartbeatResp.LeaseVersion != claimed.LeaseVersion || taskHeartbeatResp.LeaseExpiresAt == "" {
		t.Fatalf("unexpected task heartbeat response: %#v", taskHeartbeatResp)
	}

	sourceResp := getWithWorkerToken(t, endpoint+claimed.Source.Url, "secret-worker-token")
	defer sourceResp.Body.Close()
	if sourceResp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(sourceResp.Body)
		t.Fatalf("source artifact status %d: %s", sourceResp.StatusCode, string(body))
	}
	sourceBody, err := io.ReadAll(sourceResp.Body)
	if err != nil {
		t.Fatalf("read source artifact: %v", err)
	}
	if string(sourceBody) != "int main() { return 0; }\n" || sourceResp.Header.Get("X-OJOS-Artifact-Sha256") == "" {
		t.Fatalf("unexpected source artifact body/header")
	}

	packageResp := getWithWorkerToken(t, endpoint+claimed.ProblemPackage.Url, "secret-worker-token")
	defer packageResp.Body.Close()
	if packageResp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(packageResp.Body)
		t.Fatalf("problem package status %d: %s", packageResp.StatusCode, string(body))
	}
	packageBody, err := io.ReadAll(packageResp.Body)
	if err != nil {
		t.Fatalf("read problem package: %v", err)
	}
	if len(packageBody) == 0 || packageResp.Header.Get("X-OJOS-Artifact-Sha256") == "" {
		t.Fatalf("unexpected problem package artifact")
	}

	result := postJSON(t, endpoint+"/judge/worker/tasks/sub-88/result", "secret-worker-token", map[string]any{
		"worker_id":     "worker-a",
		"lease_version": claimed.LeaseVersion,
		"status":        "ACCEPTED",
		"score":         100,
		"time_ms":       13,
		"memory_kb":     2048,
		"message":       "accepted",
		"cases": []map[string]any{{
			"case_no":     1,
			"status":      "ACCEPTED",
			"score":       100,
			"time_ms":     13,
			"memory_kb":   2048,
			"stdout":      "ok\n",
			"checker_log": "matched",
		}},
	})
	defer result.Body.Close()
	if result.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(result.Body)
		t.Fatalf("result status %d: %s", result.StatusCode, string(body))
	}
	var resultResp types.WorkerSubmitResultResp
	if err := json.NewDecoder(result.Body).Decode(&resultResp); err != nil {
		t.Fatalf("decode result response: %v", err)
	}
	if !resultResp.Accepted || resultResp.Status != "ACCEPTED" {
		t.Fatalf("unexpected result response: %#v", resultResp)
	}
	if repo.succeededTaskID != "sub-88" || repo.succeededStatus != "ACCEPTED" {
		t.Fatalf("worker result should update repo, got task=%q status=%q", repo.succeededTaskID, repo.succeededStatus)
	}
	if _, ok := objects.get("88-result.json"); !ok {
		t.Fatalf("worker result should write result.json to storage-service")
	}
	entries, err := redisClient.XRange(ctx, "ojos:judge:result", "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 || entries[0].Values["task_id"] != "sub-88" || entries[0].Values["status"] != "ACCEPTED" {
		t.Fatalf("unexpected Redis result stream entries: %#v", entries)
	}
}

func TestWorkerFailRoutePublishesTerminalResultEvent(t *testing.T) {
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()

	repo := &fakeWorkerHTTPRepo{
		leases: map[string]repository.TaskLeaseView{
			"sub-91": {
				TaskID:       "sub-91",
				SubmissionID: 91,
				WorkerID:     "worker-a",
				LeaseVersion: 5,
				Status:       "RUNNING",
			},
		},
	}
	endpoint, stop := startJudgeWorkerHTTPServer(t, repo, redisClient, "", "secret-worker-token")
	defer stop()

	resp := postJSON(t, endpoint+"/judge/worker/tasks/sub-91/fail", "secret-worker-token", map[string]any{
		"worker_id":     "worker-a",
		"lease_version": 5,
		"error_type":    "SYSTEM",
		"message":       "nsjail failed",
		"retryable":     false,
	})
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		t.Fatalf("fail status %d: %s", resp.StatusCode, string(body))
	}
	var failResp types.WorkerFailTaskResp
	if err := json.NewDecoder(resp.Body).Decode(&failResp); err != nil {
		t.Fatalf("decode fail response: %v", err)
	}
	if !failResp.Accepted || failResp.Status != "SYSTEM_ERROR" {
		t.Fatalf("unexpected fail response: %#v", failResp)
	}
	if repo.failedStatus != "SYSTEM_ERROR" || repo.failedMessage != "nsjail failed" {
		t.Fatalf("failure should update repo, got status=%q message=%q", repo.failedStatus, repo.failedMessage)
	}

	entries, err := redisClient.XRange(context.Background(), "ojos:judge:result", "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 || entries[0].Values["task_id"] != "sub-91" || entries[0].Values["message"] != "nsjail failed" {
		t.Fatalf("unexpected failure result stream entries: %#v", entries)
	}
}

func startJudgeWorkerHTTPServer(
	t *testing.T,
	repo svc.WorkerTaskRepository,
	redisClient *redis.Client,
	storageEndpoint string,
	token string,
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
			Storage:    config.StorageConfig{ServiceEndpoint: storageEndpoint, Bucket: "submissions"},
			WorkerAuth: config.WorkerAuthConfig{Token: token, LeaseTTLSeconds: 45},
		},
		WorkerRepo:             repo,
		Redis:                  redisClient,
		UserContextMiddleware:  noOp,
		InternalAuthMiddleware: noOp,
		WorkerAuthMiddleware:   middleware.NewWorkerAuthMiddleware(token).Handle,
	})
	go server.Start()
	endpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForJudgeWorkerHTTPServer(t, endpoint)
	return endpoint, server.Stop
}

func waitForJudgeWorkerHTTPServer(t *testing.T, endpoint string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		resp, err := http.Get(endpoint + "/health")
		if err == nil {
			_ = resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("judge worker HTTP server did not become ready")
}

func postJSON(t *testing.T, url string, token string, body any) *http.Response {
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
	if token != "" {
		req.Header.Set("X-OJOS-Worker-Token", token)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("post %s: %v", url, err)
	}
	return resp
}

func getWithWorkerToken(t *testing.T, url string, token string) *http.Response {
	t.Helper()
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		t.Fatalf("build get request: %v", err)
	}
	if token != "" {
		req.Header.Set("X-OJOS-Worker-Token", token)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("get %s: %v", url, err)
	}
	return resp
}

func getWithServiceIdentity(t *testing.T, url string, callerService string, nodeID string, token string) *http.Response {
	t.Helper()
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		t.Fatalf("build service get request: %v", err)
	}
	req.Header.Set("X-OJOS-Caller-Service", callerService)
	req.Header.Set("X-OJOS-Node-Id", nodeID)
	req.Header.Set("Authorization", "Bearer "+token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("service get %s: %v", url, err)
	}
	return resp
}

func writeWorkerArtifacts(t *testing.T) (string, string) {
	t.Helper()
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
	return sourcePath, packageDir
}

type storageObjectMap struct {
	mu      sync.Mutex
	objects map[string][]byte
}

func (m *storageObjectMap) put(key string, body []byte) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.objects[key] = append([]byte(nil), body...)
}

func (m *storageObjectMap) get(key string) ([]byte, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	body, ok := m.objects[key]
	return append([]byte(nil), body...), ok
}

func startJudgeWorkerStorageServer(t *testing.T) (string, *storageObjectMap, func()) {
	t.Helper()
	objects := &storageObjectMap{objects: map[string][]byte{}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		key := storageRouteKey(t, r.URL.Path)
		switch r.Method {
		case http.MethodPut:
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			objects.put(key, body)
			sum := sha256.Sum256(body)
			_ = json.NewEncoder(w).Encode(map[string]any{
				"bucket":       "submissions",
				"key":          key,
				"size_bytes":   int64(len(body)),
				"sha256":       hex.EncodeToString(sum[:]),
				"content_type": r.Header.Get("Content-Type"),
			})
		case http.MethodGet:
			body, ok := objects.get(key)
			if !ok {
				http.NotFound(w, r)
				return
			}
			sum := sha256.Sum256(body)
			if bytes.HasPrefix([]byte(r.URL.Path), []byte("/api/storage/metadata/")) {
				_ = json.NewEncoder(w).Encode(map[string]any{
					"bucket":       "submissions",
					"key":          key,
					"size_bytes":   int64(len(body)),
					"sha256":       hex.EncodeToString(sum[:]),
					"content_type": "text/plain; charset=utf-8",
				})
				return
			}
			w.Header().Set("X-OJOS-Object-Sha256", hex.EncodeToString(sum[:]))
			_, _ = w.Write(body)
		default:
			t.Fatalf("unexpected storage request %s %s", r.Method, r.URL.Path)
		}
	}))
	return server.URL, objects, server.Close
}

func startJudgeInternalGatewayStorageServer(t *testing.T) (string, *storageObjectMap, func()) {
	t.Helper()
	objects := &storageObjectMap{objects: map[string][]byte{}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("X-OJOS-Caller-Service") == "" {
			t.Fatalf("internal gateway request missing caller service: %#v", r.Header)
		}
		key := internalGatewayStorageRouteKey(t, r.URL.Path)
		switch r.Method {
		case http.MethodPut:
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			objects.put(key, body)
			sum := sha256.Sum256(body)
			_ = json.NewEncoder(w).Encode(map[string]any{
				"bucket":       "submissions",
				"key":          key,
				"size_bytes":   int64(len(body)),
				"sha256":       hex.EncodeToString(sum[:]),
				"content_type": r.Header.Get("Content-Type"),
			})
		case http.MethodHead:
			body, ok := objects.get(key)
			if !ok {
				http.NotFound(w, r)
				return
			}
			sum := sha256.Sum256(body)
			w.Header().Set("Content-Length", fmt.Sprintf("%d", len(body)))
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			w.Header().Set("X-OJOS-Object-Sha256", hex.EncodeToString(sum[:]))
		case http.MethodGet:
			body, ok := objects.get(key)
			if !ok {
				http.NotFound(w, r)
				return
			}
			sum := sha256.Sum256(body)
			w.Header().Set("X-OJOS-Object-Sha256", hex.EncodeToString(sum[:]))
			_, _ = w.Write(body)
		default:
			t.Fatalf("unexpected internal gateway request %s %s", r.Method, r.URL.Path)
		}
	}))
	return server.URL, objects, server.Close
}

func storageRouteKey(t *testing.T, path string) string {
	t.Helper()
	for _, prefix := range []string{"/api/storage/objects/submissions/", "/api/storage/metadata/submissions/"} {
		if len(path) >= len(prefix) && path[:len(prefix)] == prefix {
			return path[len(prefix):]
		}
	}
	t.Fatalf("unexpected storage path %s", path)
	return ""
}

func internalGatewayStorageRouteKey(t *testing.T, path string) string {
	t.Helper()
	for _, prefix := range []string{
		"/internal/apis/storage.object.put/submissions/",
		"/internal/apis/storage.object.get/submissions/",
		"/internal/apis/storage.object.head/submissions/",
	} {
		if strings.HasPrefix(path, prefix) {
			return strings.TrimPrefix(path, prefix)
		}
	}
	t.Fatalf("unexpected internal gateway storage path %s", path)
	return ""
}

type fakeWorkerHTTPRepo struct {
	workers     map[string]repository.WorkerView
	submissions map[int64]*repository.SubmissionView
	problems    map[int64]*repository.ProblemMeta
	leases      map[string]repository.TaskLeaseView

	claimedTaskIDs []string

	succeededTaskID string
	succeededStatus string
	failedStatus    string
	failedMessage   string
}

func (r *fakeWorkerHTTPRepo) UpsertWorker(ctx context.Context, w repository.WorkerRegistration) (*repository.WorkerView, error) {
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
		RunningCount:       0,
		Status:             "ONLINE",
		Drain:              false,
		LastSeen:           time.Now(),
		RegisteredAt:       time.Now(),
		UpdatedAt:          time.Now(),
	}
	r.workers[w.WorkerID] = view
	return &view, nil
}

func (r *fakeWorkerHTTPRepo) WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*repository.WorkerView, error) {
	view, ok := r.workers[workerID]
	if !ok {
		return nil, repository.ErrWorkerNotFound
	}
	view.RunningCount = runningCount
	view.Status = "ONLINE"
	view.LastSeen = time.Now()
	view.UpdatedAt = time.Now()
	r.workers[workerID] = view
	return &view, nil
}

func (r *fakeWorkerHTTPRepo) RecoverStaleTasks(ctx context.Context) (int64, error) {
	return 0, nil
}

func (r *fakeWorkerHTTPRepo) ClaimTasks(
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
	claimed := make([]repository.TaskLeaseView, 0)
	for _, taskID := range taskIDs {
		lease, ok := r.leases[taskID]
		if !ok || lease.Status != "PENDING" || !containsString(supportedLanguages, lease.Language) {
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

func (r *fakeWorkerHTTPRepo) RefreshTaskLease(ctx context.Context, taskID string, workerID string, leaseVersion int, leaseTTL time.Duration) (*repository.TaskLeaseView, error) {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion || lease.Status != "RUNNING" {
		return nil, repository.ErrTaskLeaseInvalid
	}
	lease.LeaseExpiresAt = time.Now().Add(leaseTTL)
	lease.HeartbeatAt = time.Now()
	r.leases[taskID] = lease
	return &lease, nil
}

func (r *fakeWorkerHTTPRepo) GetTaskForLease(ctx context.Context, taskID string, workerID string, leaseVersion int) (*repository.TaskLeaseView, error) {
	lease, ok := r.leases[taskID]
	if !ok || lease.WorkerID != workerID || lease.LeaseVersion != leaseVersion {
		return nil, repository.ErrTaskLeaseInvalid
	}
	return &lease, nil
}

func (r *fakeWorkerHTTPRepo) GetSubmission(ctx context.Context, id int64) (*repository.SubmissionView, error) {
	submission, ok := r.submissions[id]
	if !ok {
		return nil, errors.New("submission not found")
	}
	return submission, nil
}

func (r *fakeWorkerHTTPRepo) GetProblemMeta(ctx context.Context, id int64) (*repository.ProblemMeta, error) {
	problem, ok := r.problems[id]
	if !ok {
		return nil, errors.New("problem not found")
	}
	return problem, nil
}

func (r *fakeWorkerHTTPRepo) MarkTaskSucceeded(
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

func (r *fakeWorkerHTTPRepo) MarkTaskFailed(
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

func containsString(items []string, value string) bool {
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

func stringSlicesEqual(left []string, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i] != right[i] {
			return false
		}
	}
	return true
}
