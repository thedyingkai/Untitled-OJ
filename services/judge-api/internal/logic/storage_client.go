package logic

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
	"path"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/config"
)

const storageScheme = "storage://"

type storageClient struct {
	endpoint        string
	internalGateway string
	getApiID        string
	putApiID        string
	headApiID       string
	client          *http.Client
}

type storageObjectMetadata struct {
	Bucket      string `json:"bucket"`
	Key         string `json:"key"`
	SizeBytes   int64  `json:"size_bytes"`
	SHA256      string `json:"sha256"`
	ContentType string `json:"content_type"`
	UpdatedAt   string `json:"updated_at"`
}

type storedSubmissionSource struct {
	CodePath   string
	CodeSha256 string
	ResultPath string
}

func storageEnabled(c config.StorageConfig) bool {
	return strings.TrimSpace(c.ServiceEndpoint) != "" || strings.TrimSpace(c.InternalGatewayEndpoint) != ""
}

func storeSubmissionSource(
	ctx context.Context,
	c config.StorageConfig,
	submissionID int64,
	language string,
	code string,
) (*storedSubmissionSource, error) {
	bucket := storageBucket(c)
	key := submissionSourceKey(submissionID, language)
	client := newStorageClient(c)
	meta, err := client.putObject(ctx, bucket, key, "text/plain; charset=utf-8", strings.NewReader(code))
	if err != nil {
		return nil, err
	}

	localDigest := sha256.Sum256([]byte(code))
	localSha := hex.EncodeToString(localDigest[:])
	if meta.SHA256 != "" && meta.SHA256 != localSha {
		return nil, fmt.Errorf("storage-service checksum mismatch for %s/%s", bucket, key)
	}

	return &storedSubmissionSource{
		CodePath:   storageRef(bucket, key),
		CodeSha256: localSha,
		ResultPath: storageRef(bucket, submissionResultKey(submissionID)),
	}, nil
}

func serveStorageArtifact(
	ctx context.Context,
	c config.StorageConfig,
	w http.ResponseWriter,
	pathRef string,
	contentType string,
) error {
	bucket, key, ok := parseStorageRef(pathRef)
	if !ok {
		return errors.New("artifact is not a storage-service ref")
	}
	client := newStorageClient(c)
	meta, body, err := client.getObject(ctx, bucket, key)
	if err != nil {
		return err
	}
	defer body.Close()

	if meta.SizeBytes < 0 || meta.SizeBytes > artifactPackageMaxSize {
		return errors.New("artifact size is invalid")
	}
	if meta.ContentType != "" {
		contentType = meta.ContentType
	}
	w.Header().Set("Content-Type", contentType)
	w.Header().Set("X-OJOS-Artifact-Sha256", meta.SHA256)
	w.Header().Set("X-OJOS-Artifact-Size", fmt.Sprintf("%d", meta.SizeBytes))
	_, err = io.Copy(w, io.LimitReader(body, artifactPackageMaxSize+1))
	return err
}

func putStorageObject(
	ctx context.Context,
	c config.StorageConfig,
	pathRef string,
	contentType string,
	body io.Reader,
) error {
	bucket, key, ok := parseStorageRef(pathRef)
	if !ok {
		return errors.New("object path is not a storage-service ref")
	}
	client := newStorageClient(c)
	_, err := client.putObject(ctx, bucket, key, contentType, body)
	return err
}

func readStorageObject(
	ctx context.Context,
	c config.StorageConfig,
	pathRef string,
	maxBytes int64,
) ([]byte, error) {
	bucket, key, ok := parseStorageRef(pathRef)
	if !ok {
		return nil, errors.New("object path is not a storage-service ref")
	}
	client := newStorageClient(c)
	meta, body, err := client.getObject(ctx, bucket, key)
	if err != nil {
		return nil, err
	}
	defer body.Close()

	if maxBytes > 0 && meta.SizeBytes > maxBytes {
		return nil, errors.New("storage object exceeds read limit")
	}
	limit := maxBytes
	if limit <= 0 {
		limit = artifactPackageMaxSize
	}
	data, err := io.ReadAll(io.LimitReader(body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, errors.New("storage object exceeds read limit")
	}
	return data, nil
}

func storageBucket(c config.StorageConfig) string {
	if bucket := strings.TrimSpace(c.Bucket); bucket != "" {
		return bucket
	}
	return "submissions"
}

func storageRef(bucket string, key string) string {
	return storageScheme + bucket + "/" + key
}

func parseStorageRef(value string) (string, string, bool) {
	value = strings.TrimSpace(value)
	if !strings.HasPrefix(value, storageScheme) {
		return "", "", false
	}
	rest := strings.TrimPrefix(value, storageScheme)
	bucket, key, ok := strings.Cut(rest, "/")
	if !ok || strings.TrimSpace(bucket) == "" || strings.TrimSpace(key) == "" {
		return "", "", false
	}
	return bucket, key, true
}

func submissionSourceKey(submissionID int64, language string) string {
	filename, err := sourceFilenameForArtifact(language)
	if err != nil {
		filename = "main.txt"
	}
	return fmt.Sprintf("%d-source-%s", submissionID, filename)
}

func submissionResultKey(submissionID int64) string {
	return fmt.Sprintf("%d-result.json", submissionID)
}

func sourceFilenameForArtifact(language string) (string, error) {
	switch strings.TrimSpace(language) {
	case "", "cpp", "cpp17", "cpp20":
		return "main.cpp", nil
	case "c", "c11", "c17":
		return "main.c", nil
	case "java", "java17":
		return "Main.java", nil
	case "python", "python3", "py3":
		return "main.py", nil
	default:
		return "", fmt.Errorf("unsupported language: %s", language)
	}
}

func newStorageClient(config config.StorageConfig) storageClient {
	return storageClient{
		endpoint:        strings.TrimRight(strings.TrimSpace(config.ServiceEndpoint), "/"),
		internalGateway: strings.TrimRight(strings.TrimSpace(config.InternalGatewayEndpoint), "/"),
		getApiID:        firstNonEmpty(config.GetApiID, "storage.object.get"),
		putApiID:        firstNonEmpty(config.PutApiID, "storage.object.put"),
		headApiID:       firstNonEmpty(config.HeadApiID, "storage.object.head"),
		client: &http.Client{
			Timeout: 15 * time.Second,
		},
	}
}

func (c storageClient) putObject(
	ctx context.Context,
	bucket string,
	key string,
	contentType string,
	body io.Reader,
) (*storageObjectMetadata, error) {
	if c.baseEndpoint() == "" {
		return nil, errors.New("storage-service endpoint is empty")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, c.putURL(bucket, key), body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", contentType)
	resp, err := c.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, storageHTTPError("put object", resp)
	}
	var meta storageObjectMetadata
	if err := json.NewDecoder(resp.Body).Decode(&meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

func (c storageClient) getObject(
	ctx context.Context,
	bucket string,
	key string,
) (*storageObjectMetadata, io.ReadCloser, error) {
	if c.baseEndpoint() == "" {
		return nil, nil, errors.New("storage-service endpoint is empty")
	}
	meta, err := c.getMetadata(ctx, bucket, key)
	if err != nil {
		return nil, nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.objectURL(bucket, key), nil)
	if err != nil {
		return nil, nil, err
	}
	resp, err := c.client.Do(req)
	if err != nil {
		return nil, nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		defer resp.Body.Close()
		return nil, nil, storageHTTPError("get object", resp)
	}
	return meta, resp.Body, nil
}

func (c storageClient) getMetadata(
	ctx context.Context,
	bucket string,
	key string,
) (*storageObjectMetadata, error) {
	if c.internalGateway != "" {
		return c.headObject(ctx, bucket, key)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.metadataURL(bucket, key), nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, storageHTTPError("get metadata", resp)
	}
	var meta storageObjectMetadata
	if err := json.NewDecoder(resp.Body).Decode(&meta); err != nil {
		return nil, err
	}
	return &meta, nil
}

func (c storageClient) headObject(
	ctx context.Context,
	bucket string,
	key string,
) (*storageObjectMetadata, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodHead, c.headURL(bucket, key), nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, storageHTTPError("head object", resp)
	}
	size, _ := strconv.ParseInt(resp.Header.Get("Content-Length"), 10, 64)
	return &storageObjectMetadata{
		Bucket:      bucket,
		Key:         cleanStorageKey(key),
		SizeBytes:   size,
		SHA256:      resp.Header.Get("X-OJOS-Object-Sha256"),
		ContentType: resp.Header.Get("Content-Type"),
	}, nil
}

func (c storageClient) objectURL(bucket string, key string) string {
	if c.internalGateway != "" {
		apiID := c.getApiID
		return c.apiURL(apiID, bucket, key)
	}
	return c.endpoint + "/api/storage/objects/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
}

func (c storageClient) metadataURL(bucket string, key string) string {
	return c.endpoint + "/api/storage/metadata/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
}

func (c storageClient) putURL(bucket string, key string) string {
	if c.internalGateway != "" {
		return c.apiURL(c.putApiID, bucket, key)
	}
	return c.objectURL(bucket, key)
}

func (c storageClient) headURL(bucket string, key string) string {
	return c.apiURL(c.headApiID, bucket, key)
}

func (c storageClient) apiURL(apiID string, bucket string, key string) string {
	return c.internalGateway + "/internal/apis/" + url.PathEscape(apiID) + "/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
}

func (c storageClient) baseEndpoint() string {
	return firstNonEmpty(c.internalGateway, c.endpoint)
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return strings.TrimSpace(value)
		}
	}
	return ""
}

func cleanStorageKey(key string) string {
	key = path.Clean("/" + strings.TrimSpace(key))
	key = strings.TrimPrefix(key, "/")
	if key == "." || key == "" {
		return "object"
	}
	return strings.ReplaceAll(key, "/", "-")
}

func storageHTTPError(action string, resp *http.Response) error {
	var buf bytes.Buffer
	_, _ = io.Copy(&buf, io.LimitReader(resp.Body, 1024))
	message := strings.TrimSpace(buf.String())
	if message == "" {
		message = resp.Status
	}
	if resp.StatusCode == http.StatusNotFound {
		return fmt.Errorf("%w: storage-service %s failed: %s", errStorageObjectNotFound, action, message)
	}
	return fmt.Errorf("storage-service %s failed: %s", action, message)
}

var errStorageObjectNotFound = errors.New("storage object not found")
