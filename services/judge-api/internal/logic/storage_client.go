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
	"ojos-judge-api/internal/submissionfs"
	"ojos-shared/servicecontext"
)

const storageScheme = "storage://"

type storageClient struct {
	endpoint        string
	internalGateway string
	getApiID        string
	putApiID        string
	headApiID       string
	callerService   string
	callerNodeID    string
	serviceToken    string
	client          *http.Client
	managed         *servicecontext.ContextProvider
	managedErr      error
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
	// ServiceEndpoint is a legacy fallback; internal gateway + api_id is the default path.
	return c.ContextProvider() != nil || strings.TrimSpace(c.ServiceEndpoint) != "" || strings.TrimSpace(c.InternalGatewayEndpoint) != ""
}

func storeSubmissionSource(
	ctx context.Context,
	c config.StorageConfig,
	submissionID int64,
	sourceFile string,
	code string,
) (*storedSubmissionSource, error) {
	bucket := storageBucket(c)
	key := submissionSourceKey(submissionID, sourceFile)
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
	w.Header().Set("Content-Length", fmt.Sprintf("%d", meta.SizeBytes))
	written, err := io.CopyN(w, body, meta.SizeBytes)
	if err != nil {
		return fmt.Errorf("stream artifact after %d of %d bytes: %w", written, meta.SizeBytes, err)
	}
	if written != meta.SizeBytes {
		return fmt.Errorf("stream artifact: wrote %d of %d bytes", written, meta.SizeBytes)
	}
	return nil
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

func submissionSourceKey(submissionID int64, sourceFile string) string {
	filename, err := submissionfs.SourceFilename(sourceFile)
	if err != nil {
		filename = "main.txt"
	}
	return fmt.Sprintf("%d-source-%s", submissionID, filename)
}

func submissionResultKey(submissionID int64) string {
	return fmt.Sprintf("%d-result.json", submissionID)
}

func newStorageClient(config config.StorageConfig) storageClient {
	result := storageClient{
		endpoint:        strings.TrimRight(strings.TrimSpace(config.ServiceEndpoint), "/"),
		internalGateway: strings.TrimRight(strings.TrimSpace(config.InternalGatewayEndpoint), "/"),
		getApiID:        firstNonEmpty(config.GetApiID, "storage.object.get"),
		putApiID:        firstNonEmpty(config.PutApiID, "storage.object.put"),
		headApiID:       firstNonEmpty(config.HeadApiID, "storage.object.head"),
		callerService:   firstNonEmpty(config.CallerService, "judge-api"),
		callerNodeID:    strings.TrimSpace(config.CallerNodeID),
		serviceToken:    strings.TrimSpace(config.ServiceToken),
		client: &http.Client{
			Timeout: 15 * time.Second,
		},
	}
	managed := config.ContextProvider()
	if managed == nil {
		return result
	}
	snapshot, err := managed.Current(context.Background())
	if err != nil {
		result.managedErr = err
		return result
	}
	if err := snapshot.RequireService("judge-api"); err != nil {
		result.managedErr = err
		return result
	}
	for _, requirement := range []string{"storage.object.get", "storage.object.put", "storage.object.head"} {
		if _, err := managed.Binding(context.Background(), requirement); err != nil {
			result.managedErr = fmt.Errorf("judge-api managed storage: %w", err)
			return result
		}
	}
	client, err := managed.Client(context.Background())
	if err != nil {
		result.managedErr = err
		return result
	}
	result.managed = managed
	result.client = client
	return result
}

func (c storageClient) putObject(
	ctx context.Context,
	bucket string,
	key string,
	contentType string,
	body io.Reader,
) (*storageObjectMetadata, error) {
	if c.managedErr != nil {
		return nil, c.managedErr
	}
	if c.baseEndpoint() == "" {
		return nil, errors.New("storage-service endpoint is empty")
	}
	req, err := c.newObjectRequest(ctx, "storage.object.put", http.MethodPut, bucket, key, body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", contentType)
	c.addInternalGatewayHeaders(req)
	resp, err := c.doRequest(ctx, req)
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
	if c.managedErr != nil {
		return nil, nil, c.managedErr
	}
	if c.baseEndpoint() == "" {
		return nil, nil, errors.New("storage-service endpoint is empty")
	}
	meta, err := c.getMetadata(ctx, bucket, key)
	if err != nil {
		return nil, nil, err
	}
	req, err := c.newObjectRequest(ctx, "storage.object.get", http.MethodGet, bucket, key, nil)
	if err != nil {
		return nil, nil, err
	}
	c.addInternalGatewayHeaders(req)
	resp, err := c.doRequest(ctx, req)
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
	if c.managedErr != nil {
		return nil, c.managedErr
	}
	if c.managed != nil || c.internalGateway != "" {
		return c.headObject(ctx, bucket, key)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.metadataURL(bucket, key), nil)
	if err != nil {
		return nil, err
	}
	c.addInternalGatewayHeaders(req)
	resp, err := c.doRequest(ctx, req)
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
	if c.managedErr != nil {
		return nil, c.managedErr
	}
	req, err := c.newObjectRequest(ctx, "storage.object.head", http.MethodHead, bucket, key, nil)
	if err != nil {
		return nil, err
	}
	c.addInternalGatewayHeaders(req)
	resp, err := c.doRequest(ctx, req)
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
	if c.managed != nil {
		value, _ := c.managed.BindingURL(context.Background(), "storage.object.get", c.relativeObjectPath(bucket, key))
		return value
	}
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
	if c.managed != nil {
		value, _ := c.managed.BindingURL(context.Background(), "storage.object.put", c.relativeObjectPath(bucket, key))
		return value
	}
	if c.internalGateway != "" {
		return c.apiURL(c.putApiID, bucket, key)
	}
	return c.objectURL(bucket, key)
}

func (c storageClient) headURL(bucket string, key string) string {
	if c.managed != nil {
		value, _ := c.managed.BindingURL(context.Background(), "storage.object.head", c.relativeObjectPath(bucket, key))
		return value
	}
	return c.apiURL(c.headApiID, bucket, key)
}

func (c storageClient) relativeObjectPath(bucket string, key string) string {
	return "/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
}

func (c storageClient) newObjectRequest(ctx context.Context, binding, method, bucket, key string, body io.Reader) (*http.Request, error) {
	if c.managedErr != nil {
		return nil, c.managedErr
	}
	if c.managed != nil {
		return c.managed.NewRequest(ctx, binding, method, c.relativeObjectPath(bucket, key), body)
	}
	var target string
	switch binding {
	case "storage.object.put":
		target = c.putURL(bucket, key)
	case "storage.object.head":
		target = c.headURL(bucket, key)
	default:
		target = c.objectURL(bucket, key)
	}
	return http.NewRequestWithContext(ctx, method, target, body)
}

func (c storageClient) apiURL(apiID string, bucket string, key string) string {
	return c.internalGateway + "/internal/apis/" + url.PathEscape(apiID) + "/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
}

func (c storageClient) baseEndpoint() string {
	if c.managed != nil {
		snapshot, err := c.managed.Current(context.Background())
		if err != nil {
			return ""
		}
		return snapshot.Gateway.Origin
	}
	return firstNonEmpty(c.internalGateway, c.endpoint)
}

// doRequest rebuilds the managed HTTP transport from the current snapshot for
// every call. Together with ContextProvider.NewRequest this makes gateway, CA,
// binding and credential rotation effective without restarting judge-api.
func (c storageClient) doRequest(ctx context.Context, req *http.Request) (*http.Response, error) {
	client := c.client
	if c.managed != nil {
		current, err := c.managed.Client(ctx)
		if err != nil {
			return nil, err
		}
		client = current
	}
	if client == nil {
		return nil, errors.New("storage HTTP client is unavailable")
	}
	return client.Do(req)
}

func (c storageClient) addInternalGatewayHeaders(req *http.Request) {
	if c.managed != nil || c.internalGateway == "" {
		return
	}
	if c.callerService != "" {
		req.Header.Set("X-OJOS-Caller-Service", c.callerService)
	}
	if c.callerNodeID != "" {
		req.Header.Set("X-OJOS-Node-Id", c.callerNodeID)
		req.Header.Set("X-OJOS-Caller-Node-Id", c.callerNodeID)
	}
	if c.serviceToken != "" {
		req.Header.Set("Authorization", "Bearer "+c.serviceToken)
	}
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
