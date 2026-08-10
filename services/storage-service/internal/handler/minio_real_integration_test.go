package handler

import (
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"strings"
	"testing"

	sharedmw "ojos-shared/middleware"
	"ojos-storage-service/internal/config"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"

	"github.com/zeromicro/go-zero/rest"
)

func TestRealMinIOHTTPObjectLifecycle(t *testing.T) {
	endpoint := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_ENDPOINT"))
	accessKey := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_ACCESS_KEY"))
	secretKey := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_SECRET_KEY"))
	if endpoint == "" || accessKey == "" || secretKey == "" {
		t.Skip("set OJOS_REAL_MINIO_ENDPOINT, OJOS_REAL_MINIO_ACCESS_KEY, and OJOS_REAL_MINIO_SECRET_KEY to run real MinIO integration")
	}

	storageEndpoint, stop := startStorageHTTPServerWithConfig(t, config.Config{
		Storage: config.StorageConfig{
			Backend: "minio",
			Buckets: []string{"submissions", "problems", "judge-artifacts"},
			MinIO: config.MinIOConfig{
				Endpoint:  endpoint,
				AccessKey: accessKey,
				SecretKey: secretKey,
				UseSSL:    parseTestBool(os.Getenv("OJOS_REAL_MINIO_USE_SSL")),
			},
		},
	})
	defer stop()

	assertStorageHealthBackend(t, storageEndpoint, "minio")
	assertStorageObjectLifecycle(t, storageEndpoint, "submissions", "real-minio-submission.txt", "source via real minio")
	assertStorageObjectLifecycle(t, storageEndpoint, "problems", "real-minio-problem.txt", "problem via real minio")
	assertStorageObjectLifecycle(t, storageEndpoint, "judge-artifacts", "real-minio-artifact.txt", "artifact via real minio")
	assertStorageObjectLifecycle(t, storageEndpoint, "problems", "real-minio-empty.in", "")
}

func startStorageHTTPServerWithConfig(t *testing.T, cfg config.Config) (string, func()) {
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
	RegisterHandlers(server, svc.NewServiceContext(cfg))
	go server.Start()
	storageEndpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForStorageHTTPServer(t, storageEndpoint)
	return storageEndpoint, server.Stop
}

func assertStorageHealthBackend(t *testing.T, endpoint string, backend string) {
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
	if health.Backend != backend {
		t.Fatalf("expected %s storage backend, got %#v", backend, health)
	}
}

func parseTestBool(value string) bool {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "1", "true", "yes", "on":
		return true
	default:
		return false
	}
}
