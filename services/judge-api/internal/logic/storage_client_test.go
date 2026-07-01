package logic

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
)

func TestStoreSubmissionSourceUsesStorageService(t *testing.T) {
	var storedPath string
	var storedBody string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPut {
			t.Fatalf("unexpected method %s", r.Method)
		}
		if r.URL.Path != "/api/storage/objects/submissions/42-source-main.cpp" {
			t.Fatalf("unexpected path %s", r.URL.Path)
		}
		storedPath = r.URL.Path
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatal(err)
		}
		storedBody = string(body)
		sum := sha256.Sum256([]byte(storedBody))
		_ = json.NewEncoder(w).Encode(storageObjectMetadata{
			Bucket:      "submissions",
			Key:         "42-source-main.cpp",
			SizeBytes:   int64(len(storedBody)),
			SHA256:      hex.EncodeToString(sum[:]),
			ContentType: "text/plain; charset=utf-8",
		})
	}))
	defer server.Close()

	stored, err := storeSubmissionSource(
		context.Background(),
		config.StorageConfig{ServiceEndpoint: server.URL, Bucket: "submissions"},
		42,
		"cpp17",
		"int main() { return 0; }",
	)
	if err != nil {
		t.Fatalf("storeSubmissionSource returned error: %v", err)
	}
	if storedPath == "" || storedBody == "" {
		t.Fatalf("storage-service was not called")
	}
	if stored.CodePath != "storage://submissions/42-source-main.cpp" {
		t.Fatalf("unexpected code path %q", stored.CodePath)
	}
	if stored.ResultPath != "storage://submissions/42-result.json" {
		t.Fatalf("unexpected result path %q", stored.ResultPath)
	}
}

func TestStorageClientCanUseInternalGatewayAPIIDs(t *testing.T) {
	var paths []string
	var methods []string
	var authHeaders []string
	var callerServices []string
	var callerNodes []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		paths = append(paths, r.URL.Path)
		methods = append(methods, r.Method)
		authHeaders = append(authHeaders, r.Header.Get("Authorization"))
		callerServices = append(callerServices, r.Header.Get("X-OJOS-Caller-Service"))
		callerNodes = append(callerNodes, r.Header.Get("X-OJOS-Node-Id"))
		switch r.Method {
		case http.MethodPut:
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			sum := sha256.Sum256(body)
			_ = json.NewEncoder(w).Encode(storageObjectMetadata{
				Bucket:      "submissions",
				Key:         "42-source-main.cpp",
				SizeBytes:   int64(len(body)),
				SHA256:      hex.EncodeToString(sum[:]),
				ContentType: r.Header.Get("Content-Type"),
			})
		case http.MethodHead:
			w.Header().Set("Content-Length", "12")
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			w.Header().Set("X-OJOS-Object-Sha256", "digest")
		case http.MethodGet:
			_, _ = w.Write([]byte("stored code"))
		default:
			t.Fatalf("unexpected method %s", r.Method)
		}
	}))
	defer server.Close()

	client := newStorageClient(config.StorageConfig{
		InternalGatewayEndpoint: server.URL,
		PutApiID:                "storage.object.put",
		GetApiID:                "storage.object.get",
		HeadApiID:               "storage.object.head",
		CallerService:           "judge-api",
		CallerNodeID:            "child-node",
		ServiceToken:            "internal-token",
	})
	if _, err := client.putObject(context.Background(), "submissions", "42-source-main.cpp", "text/plain; charset=utf-8", strings.NewReader("stored code")); err != nil {
		t.Fatalf("putObject returned error: %v", err)
	}
	meta, body, err := client.getObject(context.Background(), "submissions", "42-source-main.cpp")
	if err != nil {
		t.Fatalf("getObject returned error: %v", err)
	}
	defer body.Close()
	if meta.SHA256 != "digest" {
		t.Fatalf("head metadata was not used: %#v", meta)
	}
	data, err := io.ReadAll(body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if string(data) != "stored code" {
		t.Fatalf("unexpected body %q", string(data))
	}

	wantMethods := []string{http.MethodPut, http.MethodHead, http.MethodGet}
	wantPaths := []string{
		"/internal/apis/storage.object.put/submissions/42-source-main.cpp",
		"/internal/apis/storage.object.head/submissions/42-source-main.cpp",
		"/internal/apis/storage.object.get/submissions/42-source-main.cpp",
	}
	if strings.Join(methods, ",") != strings.Join(wantMethods, ",") {
		t.Fatalf("unexpected methods %#v", methods)
	}
	if strings.Join(paths, ",") != strings.Join(wantPaths, ",") {
		t.Fatalf("unexpected resolver paths %#v", paths)
	}
	for i := range methods {
		if authHeaders[i] != "Bearer internal-token" ||
			callerServices[i] != "judge-api" ||
			callerNodes[i] != "child-node" {
			t.Fatalf("internal gateway request %d missing service identity headers: auth=%q service=%q node=%q", i, authHeaders[i], callerServices[i], callerNodes[i])
		}
	}
}

func TestServeStorageArtifactProxiesStorageServiceObject(t *testing.T) {
	const body = "print('ok')\n"
	sum := sha256.Sum256([]byte(body))
	digest := hex.EncodeToString(sum[:])
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/api/storage/metadata/submissions/9-source-main.py":
			_ = json.NewEncoder(w).Encode(storageObjectMetadata{
				Bucket:      "submissions",
				Key:         "9-source-main.py",
				SizeBytes:   int64(len(body)),
				SHA256:      digest,
				ContentType: "text/plain; charset=utf-8",
			})
		case r.Method == http.MethodGet && r.URL.Path == "/api/storage/objects/submissions/9-source-main.py":
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			_, _ = w.Write([]byte(body))
		default:
			t.Fatalf("unexpected request %s %s", r.Method, r.URL.Path)
		}
	}))
	defer server.Close()

	recorder := httptest.NewRecorder()
	err := serveStorageArtifact(
		context.Background(),
		config.StorageConfig{ServiceEndpoint: server.URL},
		recorder,
		"storage://submissions/9-source-main.py",
		"text/plain; charset=utf-8",
	)
	if err != nil {
		t.Fatalf("serveStorageArtifact returned error: %v", err)
	}
	if recorder.Body.String() != body {
		t.Fatalf("unexpected proxied body %q", recorder.Body.String())
	}
	if recorder.Header().Get("X-OJOS-Artifact-Sha256") != digest {
		t.Fatalf("missing artifact digest header")
	}
}

func TestWorkerResultArtifactsUseStorageService(t *testing.T) {
	objects := map[string][]byte{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodPut:
			if r.URL.Path[:len("/api/storage/objects/")] != "/api/storage/objects/" {
				t.Fatalf("unexpected put path %s", r.URL.Path)
			}
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
			_, _ = w.Write(body)
		default:
			t.Fatalf("unexpected method %s", r.Method)
		}
	}))
	defer server.Close()

	storage := config.StorageConfig{ServiceEndpoint: server.URL, Bucket: "submissions"}
	err := writeWorkerResultArtifacts(
		context.Background(),
		storage,
		&repository.SubmissionView{ID: 77, ResultPath: "storage://submissions/77-result.json"},
		&types.WorkerSubmitResultReq{
			Status:   "WRONG_ANSWER",
			Score:    10,
			TimeMs:   33,
			MemoryKb: 4096,
			Cases: []types.WorkerResultCase{
				{
					CaseNo:     1,
					Status:     "WRONG_ANSWER",
					Score:      10,
					TimeMs:     33,
					MemoryKb:   4096,
					Stdout:     "stdout body",
					Stderr:     "stderr body",
					CheckerLog: "checker body",
					Message:    "mismatch",
				},
			},
		},
	)
	if err != nil {
		t.Fatalf("writeWorkerResultArtifacts returned error: %v", err)
	}

	if _, ok := objects["77-result.json"]; !ok {
		t.Fatalf("result json was not written to storage-service")
	}
	cases, err := readResultCasesWithStorage(context.Background(), storage, "storage://submissions/77-result.json")
	if err != nil {
		t.Fatalf("readResultCasesWithStorage returned error: %v", err)
	}
	if len(cases) != 1 || cases[0].Status != "WRONG_ANSWER" {
		t.Fatalf("unexpected cases: %#v", cases)
	}
	result, err := readResultFileWithStorage(context.Background(), storage, "storage://submissions/77-result.json")
	if err != nil {
		t.Fatalf("readResultFileWithStorage returned error: %v", err)
	}
	stdout, truncated, err := readTruncatedTextWithStorage(context.Background(), storage, result.Cases[0].StdoutPath, 1024)
	if err != nil {
		t.Fatalf("readTruncatedTextWithStorage returned error: %v", err)
	}
	if truncated || stdout != "stdout body" {
		t.Fatalf("unexpected stdout %q truncated=%v", stdout, truncated)
	}
}

func TestWorkerResultFlowWritesStorageAndRedisResultStream(t *testing.T) {
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

	storage := config.StorageConfig{ServiceEndpoint: storageServer.URL, Bucket: "submissions"}
	req := &types.WorkerSubmitResultReq{
		TaskId:       "sub-88",
		WorkerId:     "worker-a",
		LeaseVersion: 4,
		Status:       "ACCEPTED",
		Score:        100,
		TimeMs:       15,
		MemoryKb:     2048,
		Message:      "accepted",
		Cases: []types.WorkerResultCase{{
			CaseNo:     1,
			Status:     "ACCEPTED",
			Score:      100,
			TimeMs:     15,
			MemoryKb:   2048,
			Stdout:     "ok\n",
			Stderr:     "",
			CheckerLog: "matched",
			Message:    "accepted",
		}},
	}
	submission := &repository.SubmissionView{ID: 88, ResultPath: "storage://submissions/88-result.json"}

	if err := writeWorkerResultArtifacts(context.Background(), storage, submission, req); err != nil {
		t.Fatalf("writeWorkerResultArtifacts returned error: %v", err)
	}
	if err := publishJudgeResultEvent(context.Background(), &svc.ServiceContext{Redis: redisClient}, req.TaskId, req.WorkerId, req); err != nil {
		t.Fatalf("publishJudgeResultEvent returned error: %v", err)
	}

	if _, ok := objects["88-result.json"]; !ok {
		t.Fatalf("result json was not written to storage-service")
	}
	result, err := readResultFileWithStorage(context.Background(), storage, "storage://submissions/88-result.json")
	if err != nil {
		t.Fatalf("readResultFileWithStorage returned error: %v", err)
	}
	if result.Status != "ACCEPTED" || len(result.Cases) != 1 {
		t.Fatalf("unexpected stored result file: %#v", result)
	}
	stdout, truncated, err := readTruncatedTextWithStorage(context.Background(), storage, result.Cases[0].StdoutPath, 1024)
	if err != nil {
		t.Fatalf("readTruncatedTextWithStorage returned error: %v", err)
	}
	if truncated || stdout != "ok\n" {
		t.Fatalf("unexpected stored stdout %q truncated=%v", stdout, truncated)
	}

	entries, err := redisClient.XRange(context.Background(), judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one result stream entry, got %d", len(entries))
	}
	values := entries[0].Values
	if values["task_id"] != "sub-88" || values["submission_id"] != "88" || values["status"] != "ACCEPTED" {
		t.Fatalf("unexpected result stream values: %#v", values)
	}
}

func storageTestObjectKey(t *testing.T, requestPath string) string {
	t.Helper()
	for _, prefix := range []string{"/api/storage/objects/submissions/", "/api/storage/metadata/submissions/"} {
		if strings.HasPrefix(requestPath, prefix) {
			key, err := url.PathUnescape(strings.TrimPrefix(requestPath, prefix))
			if err != nil {
				t.Fatal(err)
			}
			return key
		}
	}
	t.Fatalf("unexpected storage path %s", requestPath)
	return ""
}
