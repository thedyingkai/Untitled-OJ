package handler

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"testing"
	"time"

	sharedmw "ojos-shared/middleware"
	"ojos-shared/storagecontract"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/rest"
)

func TestStorageHTTPObjectLifecycle(t *testing.T) {
	endpoint, stop := startStorageHTTPServer(t, []string{"submissions", "problems", "judge-artifacts"})
	defer stop()

	assertStorageHealthLocal(t, endpoint)
	assertStorageObjectLifecycle(t, endpoint, "submissions", "42-source-main.cpp", "int main(){}")
	assertStorageObjectLifecycle(t, endpoint, "problems", "problem-42.zip", "zip-bytes")
	assertStorageObjectLifecycle(t, endpoint, "judge-artifacts", "42-log.txt", "judge log")
	assertStorageObjectLifecycle(t, endpoint, "problems", "empty.in", "")
	assertStorageObjectLifecycleWithContentType(t, endpoint, "submissions", "42-result.json", `{"status":"ACCEPTED"}`, "application/json; charset=utf-8")
	assertStorageConditionalCreateAndList(t, endpoint)
}

func assertStorageConditionalCreateAndList(t *testing.T, endpoint string) {
	t.Helper()
	objectURL := endpoint + "/api/storage/objects/problems/package-sha256-test.zip"
	put := func(body string) *http.Response {
		req, err := http.NewRequest(http.MethodPut, objectURL, bytes.NewBufferString(body))
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("If-None-Match", "*")
		req.Header.Set("X-OJOS-Content-Sha256", fmt.Sprintf("%x", sha256.Sum256([]byte(body))))
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		return resp
	}
	first := put("immutable")
	defer first.Body.Close()
	if first.StatusCode != http.StatusOK {
		t.Fatalf("first conditional put returned %d", first.StatusCode)
	}
	second := put("replacement")
	defer second.Body.Close()
	if second.StatusCode != http.StatusPreconditionFailed {
		t.Fatalf("second conditional put returned %d", second.StatusCode)
	}
	get, err := http.Get(objectURL)
	if err != nil {
		t.Fatal(err)
	}
	defer get.Body.Close()
	body, _ := io.ReadAll(get.Body)
	if string(body) != "immutable" {
		t.Fatalf("conditional put replaced existing bytes: %q", body)
	}

	list, err := http.Get(endpoint + "/api/storage/objects/problems?prefix=package-sha256-&limit=1")
	if err != nil {
		t.Fatal(err)
	}
	defer list.Body.Close()
	if list.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(list.Body)
		t.Fatalf("list returned %d: %s", list.StatusCode, data)
	}
	var page types.ListObjectsResp
	if err := json.NewDecoder(list.Body).Decode(&page); err != nil {
		t.Fatal(err)
	}
	if len(page.Objects) != 1 || page.Objects[0].Key != "package-sha256-test.zip" {
		t.Fatalf("unexpected object page: %#v", page)
	}
}

func assertStorageHealthLocal(t *testing.T, endpoint string) {
	t.Helper()
	resp, err := http.Get(endpoint + "/health")
	if err != nil {
		t.Fatalf("health: %v", err)
	}
	defer resp.Body.Close()
	var health types.HealthResp
	if err := json.NewDecoder(resp.Body).Decode(&health); err != nil {
		t.Fatalf("decode health: %v", err)
	}
	if health.Backend != "local" {
		t.Fatalf("expected local storage backend, got %#v", health)
	}
}

func assertStorageObjectLifecycle(t *testing.T, endpoint string, bucket string, key string, body string) {
	t.Helper()
	assertStorageObjectLifecycleWithContentType(t, endpoint, bucket, key, body, "text/plain; charset=utf-8")
}

func assertStorageObjectLifecycleWithContentType(t *testing.T, endpoint string, bucket string, key string, body string, contentType string) {
	t.Helper()
	objectURL := endpoint + "/api/storage/objects/" + bucket + "/" + key
	putReq, err := http.NewRequest(http.MethodPut, objectURL, bytes.NewBufferString(body))
	if err != nil {
		t.Fatalf("build put request: %v", err)
	}
	putReq.Header.Set("Content-Type", contentType)
	putResp, err := http.DefaultClient.Do(putReq)
	if err != nil {
		t.Fatalf("put object: %v", err)
	}
	defer putResp.Body.Close()
	if putResp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(putResp.Body)
		t.Fatalf("unexpected put status %d: %s", putResp.StatusCode, string(data))
	}
	var putMeta types.ObjectMetadata
	if err := json.NewDecoder(putResp.Body).Decode(&putMeta); err != nil {
		t.Fatalf("decode put metadata: %v", err)
	}
	if putMeta.Bucket != bucket || putMeta.Key != key || putMeta.SizeBytes != int64(len(body)) {
		t.Fatalf("unexpected put metadata: %#v", putMeta)
	}
	if putMeta.SHA256 == "" {
		t.Fatalf("put metadata should include sha256")
	}

	metaResp, err := http.Get(endpoint + "/api/storage/metadata/" + bucket + "/" + key)
	if err != nil {
		t.Fatalf("get metadata: %v", err)
	}
	defer metaResp.Body.Close()
	if metaResp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected metadata status %d", metaResp.StatusCode)
	}
	var fetchedMeta types.ObjectMetadata
	if err := json.NewDecoder(metaResp.Body).Decode(&fetchedMeta); err != nil {
		t.Fatalf("decode fetched metadata: %v", err)
	}
	if fetchedMeta.SHA256 != putMeta.SHA256 {
		t.Fatalf("metadata sha mismatch: got %s want %s", fetchedMeta.SHA256, putMeta.SHA256)
	}

	getResp, err := http.Get(objectURL)
	if err != nil {
		t.Fatalf("get object: %v", err)
	}
	defer getResp.Body.Close()
	if getResp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected get status %d", getResp.StatusCode)
	}
	data, err := io.ReadAll(getResp.Body)
	if err != nil {
		t.Fatalf("read object: %v", err)
	}
	if string(data) != body {
		t.Fatalf("unexpected object body %q", string(data))
	}
	if got := getResp.Header.Get("X-OJOS-Object-Sha256"); got != putMeta.SHA256 {
		t.Fatalf("object sha header mismatch: got %q want %q", got, putMeta.SHA256)
	}
	if got := getResp.Header.Get("Content-Length"); got != fmt.Sprintf("%d", putMeta.SizeBytes) {
		t.Fatalf("get content length mismatch: got %q want %d", got, putMeta.SizeBytes)
	}

	headResp, err := http.Head(objectURL)
	if err != nil {
		t.Fatalf("head object: %v", err)
	}
	defer headResp.Body.Close()
	if headResp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected head status %d", headResp.StatusCode)
	}
	if got := headResp.Header.Get("X-OJOS-Object-Sha256"); got != putMeta.SHA256 {
		t.Fatalf("head sha header mismatch: got %q want %q", got, putMeta.SHA256)
	}
	if got := headResp.Header.Get("Content-Length"); got != fmt.Sprintf("%d", putMeta.SizeBytes) {
		t.Fatalf("head content length mismatch: got %q want %d", got, putMeta.SizeBytes)
	}
	if got := headResp.Header.Get(storagecontract.ResultHeader); got != storagecontract.ResultPresent {
		t.Fatalf("head result mismatch: got %q want %q", got, storagecontract.ResultPresent)
	}

	deleteReq, err := http.NewRequest(http.MethodDelete, objectURL, nil)
	if err != nil {
		t.Fatalf("build delete request: %v", err)
	}
	deleteReq.Header.Set("X-OJOS-Expected-Sha256", strings.Repeat("0", 64))
	deleteReq.Header.Set("X-OJOS-Expected-Size", fmt.Sprintf("%d", putMeta.SizeBytes))
	deleteResp, err := http.DefaultClient.Do(deleteReq)
	if err != nil {
		t.Fatalf("delete object: %v", err)
	}
	_ = deleteResp.Body.Close()
	if deleteResp.StatusCode != http.StatusPreconditionFailed {
		t.Fatalf("mismatched conditional delete status %d", deleteResp.StatusCode)
	}
	deleteReq, err = http.NewRequest(http.MethodDelete, objectURL, nil)
	if err != nil {
		t.Fatalf("build matching delete request: %v", err)
	}
	deleteReq.Header.Set("X-OJOS-Expected-Sha256", putMeta.SHA256)
	deleteReq.Header.Set("X-OJOS-Expected-Size", fmt.Sprintf("%d", putMeta.SizeBytes))
	deleteResp, err = http.DefaultClient.Do(deleteReq)
	if err != nil {
		t.Fatalf("conditionally delete object: %v", err)
	}
	defer deleteResp.Body.Close()
	if deleteResp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected matching delete status %d", deleteResp.StatusCode)
	}
	if got := deleteResp.Header.Get(storagecontract.ResultHeader); got != storagecontract.ResultDeleted {
		t.Fatalf("delete result mismatch: got %q want %q", got, storagecontract.ResultDeleted)
	}
	var deleteResult struct {
		Deleted bool `json:"deleted"`
	}
	if err := json.NewDecoder(deleteResp.Body).Decode(&deleteResult); err != nil || !deleteResult.Deleted {
		t.Fatalf("invalid delete acknowledgement: result=%#v err=%v", deleteResult, err)
	}

	missingResp, err := http.Get(objectURL)
	if err != nil {
		t.Fatalf("get deleted object: %v", err)
	}
	defer missingResp.Body.Close()
	if missingResp.StatusCode != http.StatusNotFound {
		t.Fatalf("deleted object GET should return 404, got %d", missingResp.StatusCode)
	}

	missingHeadResp, err := http.Head(objectURL)
	if err != nil {
		t.Fatalf("head deleted object: %v", err)
	}
	defer missingHeadResp.Body.Close()
	if missingHeadResp.StatusCode != http.StatusNotFound {
		t.Fatalf("deleted object HEAD should return 404, got %d", missingHeadResp.StatusCode)
	}
	if got := missingHeadResp.Header.Get(storagecontract.ResultHeader); got != storagecontract.ResultObjectNotFound {
		t.Fatalf("deleted object HEAD result = %q, want %q", got, storagecontract.ResultObjectNotFound)
	}
}

func startStorageHTTPServer(t *testing.T, buckets []string) (string, func()) {
	t.Helper()
	sharedmw.InstallHTTPErrorHandler()
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
	RegisterHandlers(server, svc.NewServiceContext(config.Config{
		Storage: config.StorageConfig{
			Root:    t.TempDir(),
			Buckets: buckets,
		},
	}))
	go server.Start()
	endpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForStorageHTTPServer(t, endpoint)
	return endpoint, server.Stop
}

func waitForStorageHTTPServer(t *testing.T, endpoint string) {
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
	t.Fatalf("storage HTTP server did not become ready")
}
