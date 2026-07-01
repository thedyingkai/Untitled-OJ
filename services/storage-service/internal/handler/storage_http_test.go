package handler

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"testing"
	"time"

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
	assertStorageObjectLifecycleWithContentType(t, endpoint, "submissions", "42-result.json", `{"status":"ACCEPTED"}`, "application/json; charset=utf-8")
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

	deleteReq, err := http.NewRequest(http.MethodDelete, objectURL, nil)
	if err != nil {
		t.Fatalf("build delete request: %v", err)
	}
	deleteResp, err := http.DefaultClient.Do(deleteReq)
	if err != nil {
		t.Fatalf("delete object: %v", err)
	}
	defer deleteResp.Body.Close()
	if deleteResp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected delete status %d", deleteResp.StatusCode)
	}

	missingResp, err := http.Get(objectURL)
	if err != nil {
		t.Fatalf("get deleted object: %v", err)
	}
	defer missingResp.Body.Close()
	if missingResp.StatusCode == http.StatusOK {
		t.Fatalf("deleted object should not be readable")
	}
}

func startStorageHTTPServer(t *testing.T, buckets []string) (string, func()) {
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
