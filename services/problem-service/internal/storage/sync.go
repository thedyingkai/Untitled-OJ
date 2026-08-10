package storage

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
	"ojos-shared/eventing"
	"ojos-shared/servicecontext"
)

const (
	defaultBucket        = "problems"
	defaultPutAPIID      = "storage.object.put"
	defaultHeadAPIID     = "storage.object.head"
	defaultCallerService = "problem-service"
)

// SyncProblemFiles publishes every remote authoring object behind a durable
// upload intent. The caller must resolve the returned files from the same
// transaction that persists problem_files; failed or interrupted mutations
// therefore leave a complete, reclaimable ledger rather than untracked
// content-addressed objects.
func SyncProblemFiles(ctx context.Context, cfg config.StorageConfig, problemID int64, files []packagefs.IndexedFile, intents ArtifactIntentRegistrar) ([]packagefs.IndexedFile, error) {
	managed, err := loadManagedStorageClient()
	if err != nil {
		return nil, err
	}
	if managed == nil && strings.TrimSpace(cfg.InternalGatewayEndpoint) == "" && strings.TrimSpace(cfg.ServiceEndpoint) == "" {
		return files, nil
	}
	if problemID <= 0 {
		return nil, fmt.Errorf("invalid problem id: %d", problemID)
	}

	client := &http.Client{Timeout: 15 * time.Second}
	if managed != nil {
		client = managed.client
	}
	synced := make([]packagefs.IndexedFile, 0, len(files))
	for _, file := range files {
		data, err := os.ReadFile(file.StoragePath)
		if err != nil {
			return nil, fmt.Errorf("read problem package file %s: %w", file.StoragePath, err)
		}
		digestBytes := sha256.Sum256(data)
		digest := hex.EncodeToString(digestBytes[:])
		if expected := strings.ToLower(strings.TrimSpace(file.Sha256)); expected != "" && expected != digest {
			return nil, fmt.Errorf("problem package file %s changed while staging: expected sha256 %s, got %s", file.LogicalPath, expected, digest)
		}
		// Individual authoring objects are immutable too. A failed DB commit can
		// therefore leave only an unreferenced object for GC; it cannot overwrite
		// the bytes referenced by the previously committed problem_files row.
		key := ProblemContentObjectKey(problemID, digest)
		artifact := eventing.ArtifactRef{
			URI:         "storage://" + bucket(cfg) + "/" + key,
			SHA256:      digest,
			SizeBytes:   int64(len(data)),
			ContentType: file.MimeType,
		}
		if intents == nil {
			return nil, errors.New("remote problem file publication requires a durable upload-intent registrar")
		}
		if err := intents.RegisterArtifactUploadIntent(ctx, artifact); err != nil {
			return nil, fmt.Errorf("register problem file upload intent: %w", err)
		}
		if err := putObject(ctx, managed, client, cfg, key, file.MimeType, data, digest); err != nil {
			return nil, err
		}
		if err := intents.MarkArtifactUploadCompleted(ctx, artifact); err != nil {
			return nil, fmt.Errorf("mark problem file upload completed: %w", err)
		}
		file.Sha256 = digest
		file.SizeBytes = int64(len(data))
		file.StoragePath = artifact.URI
		synced = append(synced, file)
	}
	return synced, nil
}

func ProblemContentObjectKey(problemID int64, digest string) string {
	digest = strings.TrimPrefix(strings.ToLower(strings.TrimSpace(digest)), "sha256:")
	return fmt.Sprintf("problem-%d-objects-sha256-%s", problemID, digest)
}

func ProblemObjectKey(problemID int64, logicalPath string) string {
	logicalPath = strings.Trim(strings.ReplaceAll(strings.TrimSpace(logicalPath), "\\", "/"), "/")
	if logicalPath == "" {
		logicalPath = "file"
	}
	replacer := strings.NewReplacer("/", "__", " ", "_", ":", "_")
	return fmt.Sprintf("problem-%d-%s", problemID, replacer.Replace(logicalPath))
}

func putObject(ctx context.Context, managed *managedStorageClient, client *http.Client, cfg config.StorageConfig, key string, contentType string, data []byte, digest string) error {
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	headers := http.Header{
		"Content-Type":          []string{contentType},
		"X-OJOS-Content-Sha256": []string{digest},
		"If-None-Match":         []string{"*"},
	}
	var req *http.Request
	var err error
	if managed != nil {
		relativePath := "/" + url.PathEscape(bucket(cfg)) + "/" + url.PathEscape(key)
		req, err = managed.context.NewRequestWithOptions(ctx, storagePutBinding, http.MethodPut, relativePath, bytes.NewReader(data), servicecontext.RequestOptions{Headers: headers, ContentLength: int64(len(data))})
	} else {
		target, legacyHeaders := putTarget(cfg, key)
		req, err = http.NewRequestWithContext(ctx, http.MethodPut, target, bytes.NewReader(data))
		if err == nil {
			req.ContentLength = int64(len(data))
			req.Header = headers
			for header, value := range legacyHeaders {
				if strings.TrimSpace(value) != "" {
					req.Header.Set(header, value)
				}
			}
		}
	}
	if err != nil {
		return err
	}

	resp, err := client.Do(req)
	if err != nil {
		return fmt.Errorf("put problem file to storage failed: %w", err)
	}
	defer resp.Body.Close()
	body, readErr := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
	if readErr != nil {
		return readErr
	}
	if resp.StatusCode == http.StatusPreconditionFailed {
		_, err := verifyExistingObject(ctx, managed, client, cfg, key, digest, int64(len(data)))
		return err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("put problem file to storage returned %s: %s", resp.Status, strings.TrimSpace(string(body)))
	}
	var meta objectMetadata
	if err := json.Unmarshal(body, &meta); err != nil {
		return fmt.Errorf("decode immutable problem file metadata: %w", err)
	}
	if !strings.EqualFold(strings.TrimSpace(meta.SHA256), digest) || meta.SizeBytes != int64(len(data)) {
		return fmt.Errorf(
			"storage problem file identity mismatch: expected %s/%d, got %s/%d",
			digest, len(data), meta.SHA256, meta.SizeBytes,
		)
	}
	return nil
}

func putTarget(cfg config.StorageConfig, key string) (string, map[string]string) {
	return objectTarget(cfg, key, cfg.PutApiID, defaultPutAPIID)
}

func headTarget(cfg config.StorageConfig, key string) (string, map[string]string) {
	return objectTarget(cfg, key, cfg.HeadApiID, defaultHeadAPIID)
}

func objectTarget(cfg config.StorageConfig, key, configuredAPIID, fallbackAPIID string) (string, map[string]string) {
	escapedBucket := url.PathEscape(bucket(cfg))
	escapedKey := url.PathEscape(key)
	if endpoint := strings.TrimRight(strings.TrimSpace(cfg.InternalGatewayEndpoint), "/"); endpoint != "" {
		apiID := strings.TrimSpace(configuredAPIID)
		if apiID == "" {
			apiID = fallbackAPIID
		}
		headers := map[string]string{
			"Authorization":              bearer(cfg.ServiceToken),
			"X-OJOS-Caller-Service":      callerService(cfg),
			"X-OJOS-Caller-Node-Id":      strings.TrimSpace(cfg.CallerNodeID),
			"X-OJOS-Node-Id":             strings.TrimSpace(cfg.CallerNodeID),
			"X-OJOS-Api-Id":              apiID,
			"X-OJOS-Problem-Storage-Key": key,
		}
		return endpoint + "/internal/apis/" + apiID + "/" + escapedBucket + "/" + escapedKey, headers
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
