package storage

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
)

const (
	defaultBucket        = "problems"
	defaultPutAPIID      = "storage.object.put"
	defaultCallerService = "problem-service"
)

func SyncProblemFiles(ctx context.Context, cfg config.StorageConfig, problemID int64, files []packagefs.IndexedFile) ([]packagefs.IndexedFile, error) {
	if strings.TrimSpace(cfg.InternalGatewayEndpoint) == "" && strings.TrimSpace(cfg.ServiceEndpoint) == "" {
		return files, nil
	}
	if problemID <= 0 {
		return nil, fmt.Errorf("invalid problem id: %d", problemID)
	}

	client := &http.Client{Timeout: 15 * time.Second}
	synced := make([]packagefs.IndexedFile, 0, len(files))
	for _, file := range files {
		key := ProblemObjectKey(problemID, file.LogicalPath)
		data, err := os.ReadFile(file.StoragePath)
		if err != nil {
			return nil, fmt.Errorf("read problem package file %s: %w", file.StoragePath, err)
		}
		if err := putObject(ctx, client, cfg, key, file.MimeType, data); err != nil {
			return nil, err
		}
		file.StoragePath = "storage://" + bucket(cfg) + "/" + key
		synced = append(synced, file)
	}
	return synced, nil
}

func ProblemObjectKey(problemID int64, logicalPath string) string {
	logicalPath = strings.Trim(strings.ReplaceAll(strings.TrimSpace(logicalPath), "\\", "/"), "/")
	if logicalPath == "" {
		logicalPath = "file"
	}
	replacer := strings.NewReplacer("/", "__", " ", "_", ":", "_")
	return fmt.Sprintf("problem-%d-%s", problemID, replacer.Replace(logicalPath))
}

func putObject(ctx context.Context, client *http.Client, cfg config.StorageConfig, key string, contentType string, data []byte) error {
	target, headers := putTarget(cfg, key)
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, target, bytes.NewReader(data))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", contentType)
	for header, value := range headers {
		if strings.TrimSpace(value) != "" {
			req.Header.Set(header, value)
		}
	}

	resp, err := client.Do(req)
	if err != nil {
		return fmt.Errorf("put problem file to storage failed: %w", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("put problem file to storage returned %s: %s", resp.Status, strings.TrimSpace(string(body)))
	}
	return nil
}

func putTarget(cfg config.StorageConfig, key string) (string, map[string]string) {
	escapedBucket := url.PathEscape(bucket(cfg))
	escapedKey := url.PathEscape(key)
	if endpoint := strings.TrimRight(strings.TrimSpace(cfg.InternalGatewayEndpoint), "/"); endpoint != "" {
		putAPIID := strings.TrimSpace(cfg.PutApiID)
		if putAPIID == "" {
			putAPIID = defaultPutAPIID
		}
		headers := map[string]string{
			"Authorization":              bearer(cfg.ServiceToken),
			"X-OJOS-Caller-Service":      callerService(cfg),
			"X-OJOS-Caller-Node-Id":      strings.TrimSpace(cfg.CallerNodeID),
			"X-OJOS-Node-Id":             strings.TrimSpace(cfg.CallerNodeID),
			"X-OJOS-Api-Id":              putAPIID,
			"X-OJOS-Problem-Storage-Key": key,
		}
		return endpoint + "/internal/apis/" + putAPIID + "/" + escapedBucket + "/" + escapedKey, headers
	}
	return strings.TrimRight(strings.TrimSpace(cfg.ServiceEndpoint), "/") + "/api/storage/objects/" + escapedBucket + "/" + escapedKey, map[string]string{}
}

func bucket(cfg config.StorageConfig) string {
	if value := strings.TrimSpace(cfg.Bucket); value != "" {
		return value
	}
	return defaultBucket
}

func callerService(cfg config.StorageConfig) string {
	if value := strings.TrimSpace(cfg.CallerService); value != "" {
		return value
	}
	return defaultCallerService
}

func bearer(token string) string {
	token = strings.TrimSpace(token)
	if token == "" {
		return ""
	}
	if strings.HasPrefix(strings.ToLower(token), "bearer ") {
		return token
	}
	return "Bearer " + token
}
