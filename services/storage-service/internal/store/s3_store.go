package store

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"ojos-shared/storagecontract"
	"ojos-storage-service/internal/types"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
)

type S3ObjectStore struct {
	client  *minio.Client
	backend string
	buckets map[string]struct{}
	now     func() time.Time

	// Bucket registration is independent of slow object uploads. Only mutations
	// share mu; readiness and metadata reads must not wait for an upload body.
	bucketsMu sync.RWMutex
	mu        sync.Mutex
}

const temporaryObjectPrefix = ".ojos-upload/"

// MinIOObjectStore is a source-compatible alias for the legacy constructor.
type MinIOObjectStore = S3ObjectStore

func NewMinIOObjectStore(options MinIOOptions, buckets []string) (*MinIOObjectStore, error) {
	return newS3ObjectStore(options, buckets, "minio")
}

func NewS3ObjectStore(options S3Options, buckets []string) (*S3ObjectStore, error) {
	return newS3ObjectStore(options, buckets, "s3")
}

func newS3ObjectStore(options S3Options, buckets []string, backend string) (*S3ObjectStore, error) {
	endpoint := strings.TrimSpace(options.Endpoint)
	if endpoint == "" {
		return nil, fmt.Errorf("s3 endpoint is required")
	}
	accessKey := strings.TrimSpace(options.AccessKey)
	secretKey := strings.TrimSpace(options.SecretKey)
	if accessKey == "" || secretKey == "" {
		return nil, fmt.Errorf("s3 access key and secret key are required")
	}
	client, err := minio.New(endpoint, &minio.Options{
		Creds:        credentials.NewStaticV4(accessKey, secretKey, ""),
		Secure:       options.UseSSL,
		Region:       strings.TrimSpace(options.Region),
		BucketLookup: minio.BucketLookupPath,
		MaxRetries:   1,
	})
	if err != nil {
		return nil, err
	}
	store := &S3ObjectStore{
		client:  client,
		backend: backend,
		buckets: bucketSet(buckets),
		now:     time.Now,
	}
	if err := store.ensureBuckets(context.Background()); err != nil {
		return nil, err
	}
	return store, nil
}

func (s *S3ObjectStore) Backend() string {
	return s.backend
}

func (s *S3ObjectStore) Ready(ctx context.Context) error {
	if s == nil || s.client == nil {
		return errors.New("s3 client is unavailable")
	}
	for _, bucket := range s.BucketNames() {
		exists, err := s.client.BucketExists(ctx, bucket)
		if err != nil {
			return fmt.Errorf("s3 bucket %s readiness: %w", bucket, err)
		}
		if !exists {
			return fmt.Errorf("s3 bucket %s is unavailable", bucket)
		}
	}
	return nil
}

func (s *S3ObjectStore) BucketNames() []string {
	s.bucketsMu.RLock()
	defer s.bucketsMu.RUnlock()

	names := make([]string, 0, len(s.buckets))
	for bucket := range s.buckets {
		names = append(names, bucket)
	}
	sort.Strings(names)
	return names
}

func (s *S3ObjectStore) EnsureBucket(bucket string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	created, err := s.ensureBucketLocked(context.Background(), bucket)
	if err != nil {
		return false, err
	}
	return created, nil
}

func (s *S3ObjectStore) Put(ctx context.Context, bucket, key string, options PutOptions, body io.Reader) (types.ObjectMetadata, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	key, err := cleanObjectKey(key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if _, err := s.ensureBucketLocked(ctx, bucket); err != nil {
		return types.ObjectMetadata{}, err
	}
	if options.SizeKnown && (options.SizeBytes < 0 || options.SizeBytes > maxObjectBytes) {
		return types.ObjectMetadata{}, fmt.Errorf("invalid object size %d", options.SizeBytes)
	}
	expectedSHA, err := normalizeExpectedSHA256(options.ExpectedSHA256)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if options.IfAbsent {
		if _, err := s.metadata(ctx, bucket, key); err == nil {
			return types.ObjectMetadata{}, ErrPreconditionFailed
		} else if !errors.Is(err, ErrObjectNotFound) {
			return types.ObjectMetadata{}, err
		}
	}
	temporaryKey, err := temporaryObjectKey()
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	defer func() {
		_ = s.client.RemoveObject(context.Background(), bucket, temporaryKey, minio.RemoveObjectOptions{})
	}()
	hasher := sha256.New()
	limited := io.LimitReader(body, maxObjectBytes+1)
	counted := &countingReader{reader: io.TeeReader(limited, hasher)}
	_, err = s.client.PutObject(ctx, bucket, temporaryKey, counted, -1, minio.PutObjectOptions{
		ContentType: "application/octet-stream",
		PartSize:    8 * 1024 * 1024,
	})
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	size := counted.count
	if size > maxObjectBytes {
		return types.ObjectMetadata{}, fmt.Errorf("object exceeds %d bytes", maxObjectBytes)
	}
	if options.SizeKnown && size != options.SizeBytes {
		return types.ObjectMetadata{}, fmt.Errorf("object size mismatch: expected %d, got %d", options.SizeBytes, size)
	}
	sha := hex.EncodeToString(hasher.Sum(nil))
	if expectedSHA != "" && sha != expectedSHA {
		return types.ObjectMetadata{}, fmt.Errorf("object sha256 mismatch: expected %s, got %s", expectedSHA, sha)
	}

	contentType := options.ContentType
	if contentType == "" {
		contentType = mime.TypeByExtension(filepath.Ext(key))
	}
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	meta := types.ObjectMetadata{
		Bucket:      bucket,
		Key:         key,
		SizeBytes:   size,
		SHA256:      sha,
		ContentType: contentType,
		UpdatedAt:   s.now().UTC().Format(time.RFC3339),
	}
	temporary, err := s.client.GetObject(ctx, bucket, temporaryKey, minio.GetObjectOptions{})
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	defer temporary.Close()
	putOptions := minio.PutObjectOptions{
		ContentType:      contentType,
		DisableMultipart: true,
		UserMetadata: map[string]string{
			"ojos-sha256":     sha,
			"ojos-updated-at": meta.UpdatedAt,
		},
	}
	if options.IfAbsent {
		// A single conditional PUT is the cross-process atomic boundary. The
		// source is a provider-side temporary object, so this remains compatible
		// with a read-only container root without buffering the payload in memory.
		putOptions.SetMatchETagExcept("*")
	}
	_, err = s.client.PutObject(ctx, bucket, key, temporary, size, putOptions)
	if err != nil {
		if isMinIOPreconditionFailure(err) {
			return types.ObjectMetadata{}, ErrPreconditionFailed
		}
		return types.ObjectMetadata{}, err
	}
	return meta, nil
}

func (s *S3ObjectStore) List(ctx context.Context, bucket, prefix, cursor string, limit int) (ObjectPage, error) {
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return ObjectPage{}, err
	}
	limit = normalizeListLimit(limit)
	listCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	objects := s.client.ListObjects(listCtx, bucket, minio.ListObjectsOptions{
		Prefix:       strings.TrimSpace(prefix),
		Recursive:    true,
		WithMetadata: true,
		StartAfter:   strings.TrimSpace(cursor),
		MaxKeys:      limit + 1,
	})
	items := make([]types.ObjectMetadata, 0, limit+1)
	for object := range objects {
		if object.Err != nil {
			if errors.Is(object.Err, context.Canceled) && len(items) > limit {
				continue
			}
			return ObjectPage{}, object.Err
		}
		// Upload staging is provider-owned, not part of the published object
		// namespace. Keep paging until enough business objects have been read.
		if strings.HasPrefix(object.Key, temporaryObjectPrefix) {
			continue
		}
		if len(items) <= limit {
			meta := metadataFromObjectInfo(bucket, object)
			if meta.SHA256 == "" || meta.UpdatedAt == "" || meta.SizeBytes < 0 {
				fullMeta, err := s.metadata(listCtx, bucket, object.Key)
				if err != nil {
					return ObjectPage{}, err
				}
				meta = fullMeta
			}
			items = append(items, meta)
			if len(items) > limit {
				cancel()
			}
		}
	}
	page := ObjectPage{Objects: items}
	if len(items) > limit {
		page.Objects = items[:limit]
		page.NextCursor = page.Objects[len(page.Objects)-1].Key
	}
	return page, nil
}

func (s *S3ObjectStore) Serve(w http.ResponseWriter, r *http.Request, bucket, key string) error {
	key, err := cleanObjectKey(key)
	if err != nil {
		return err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return err
	}
	meta, err := s.metadata(r.Context(), bucket, key)
	if err != nil {
		if r.Method == http.MethodHead && errors.Is(err, ErrObjectNotFound) {
			w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultObjectNotFound)
		}
		return err
	}
	if meta.ContentType != "" {
		w.Header().Set("Content-Type", meta.ContentType)
	}
	if meta.SHA256 != "" {
		w.Header().Set("X-OJOS-Object-Sha256", meta.SHA256)
	}
	w.Header().Set("Content-Length", fmt.Sprintf("%d", meta.SizeBytes))
	if r.Method == http.MethodHead {
		w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultPresent)
		return nil
	}
	object, err := s.client.GetObject(r.Context(), bucket, key, minio.GetObjectOptions{})
	if err != nil {
		return err
	}
	defer object.Close()
	written, err := io.CopyN(w, object, meta.SizeBytes)
	if err != nil {
		return fmt.Errorf("stream S3 object %s/%s after %d of %d bytes: %w", bucket, key, written, meta.SizeBytes, err)
	}
	if written != meta.SizeBytes {
		return fmt.Errorf("stream S3 object %s/%s: wrote %d of %d bytes", bucket, key, written, meta.SizeBytes)
	}
	return nil
}

func (s *S3ObjectStore) Delete(bucket, key string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	key, err := cleanObjectKey(key)
	if err != nil {
		return err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return err
	}
	return s.client.RemoveObject(context.Background(), bucket, key, minio.RemoveObjectOptions{})
}

func (s *S3ObjectStore) DeleteIfMatches(ctx context.Context, bucket, key, expectedSHA256 string, expectedSize int64) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	expectedSHA256, err := normalizeExpectedSHA256(expectedSHA256)
	if err != nil || expectedSHA256 == "" || expectedSize < 0 {
		return ErrPreconditionFailed
	}
	meta, err := s.metadata(ctx, bucket, key)
	if err != nil {
		if errors.Is(err, ErrObjectNotFound) {
			return nil
		}
		return err
	}
	if meta.SHA256 != expectedSHA256 || meta.SizeBytes != expectedSize {
		return ErrPreconditionFailed
	}
	// All writes through this provider share mu. The key is content-addressed,
	// and S3 access is not exposed to consumers, so the stat/remove pair is
	// the provider's atomic conditional-delete boundary.
	return s.client.RemoveObject(ctx, bucket, key, minio.RemoveObjectOptions{})
}

func (s *S3ObjectStore) Metadata(bucket, key string) (types.ObjectMetadata, error) {
	return s.metadata(context.Background(), bucket, key)
}

func (s *S3ObjectStore) metadata(ctx context.Context, bucket, key string) (types.ObjectMetadata, error) {
	key, err := cleanObjectKey(key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return types.ObjectMetadata{}, err
	}
	info, err := s.client.StatObject(ctx, bucket, key, minio.StatObjectOptions{})
	if err != nil {
		if isMinIOObjectNotFound(err) {
			bucketExists, bucketErr := s.client.BucketExists(ctx, bucket)
			if bucketErr != nil {
				return types.ObjectMetadata{}, bucketErr
			}
			if bucketExists {
				return types.ObjectMetadata{}, ErrObjectNotFound
			}
		}
		return types.ObjectMetadata{}, err
	}
	meta := metadataFromObjectInfo(bucket, info)
	if meta.Key == "" {
		meta.Key = key
	}
	return meta, nil
}

func (s *S3ObjectStore) ensureBuckets(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	for _, bucket := range s.BucketNames() {
		if _, err := s.ensureBucketLocked(ctx, bucket); err != nil {
			return err
		}
	}
	return nil
}

func (s *S3ObjectStore) ensureBucketLocked(ctx context.Context, bucket string) (bool, error) {
	if err := validateBucket(bucket); err != nil {
		return false, err
	}
	existed, err := s.client.BucketExists(ctx, bucket)
	if err != nil {
		return false, err
	}
	if !existed {
		if err := s.client.MakeBucket(ctx, bucket, minio.MakeBucketOptions{}); err != nil {
			return false, err
		}
	}
	s.bucketsMu.Lock()
	defer s.bucketsMu.Unlock()
	_, configured := s.buckets[bucket]
	s.buckets[bucket] = struct{}{}
	return !configured || !existed, nil
}

func (s *S3ObjectStore) ensureConfiguredBucket(bucket string) error {
	if err := validateBucket(bucket); err != nil {
		return err
	}
	s.bucketsMu.RLock()
	defer s.bucketsMu.RUnlock()
	if _, ok := s.buckets[bucket]; !ok {
		return fmt.Errorf("bucket %s is not configured", bucket)
	}
	return nil
}

func bucketSet(buckets []string) map[string]struct{} {
	set := make(map[string]struct{}, len(buckets))
	for _, bucket := range buckets {
		if bucket = strings.TrimSpace(bucket); bucket != "" {
			set[bucket] = struct{}{}
		}
	}
	return set
}

type countingReader struct {
	reader io.Reader
	count  int64
}

func (reader *countingReader) Read(buffer []byte) (int, error) {
	n, err := reader.reader.Read(buffer)
	reader.count += int64(n)
	return n, err
}

func temporaryObjectKey() (string, error) {
	random := make([]byte, 18)
	if _, err := rand.Read(random); err != nil {
		return "", fmt.Errorf("generate temporary object identity: %w", err)
	}
	return temporaryObjectPrefix + hex.EncodeToString(random), nil
}

func isMinIOObjectNotFound(err error) bool {
	response := minio.ToErrorResponse(err)
	switch strings.ToLower(strings.TrimSpace(response.Code)) {
	case "nosuchkey", "nosuchobject":
		return true
	case "notfound":
		return response.StatusCode == http.StatusNotFound
	default:
		return false
	}
}

func isMinIOPreconditionFailure(err error) bool {
	response := minio.ToErrorResponse(err)
	return response.StatusCode == http.StatusPreconditionFailed ||
		strings.EqualFold(strings.TrimSpace(response.Code), minio.PreconditionFailed)
}

func metadataFromObjectInfo(bucket string, info minio.ObjectInfo) types.ObjectMetadata {
	contentType := info.ContentType
	if contentType == "" {
		contentType = info.Metadata.Get("Content-Type")
	}
	updatedAt := userMetadataValue(info, "ojos-updated-at")
	if updatedAt == "" && !info.LastModified.IsZero() {
		updatedAt = info.LastModified.UTC().Format(time.RFC3339)
	}
	return types.ObjectMetadata{
		Bucket:      bucket,
		Key:         info.Key,
		SizeBytes:   info.Size,
		SHA256:      userMetadataValue(info, "ojos-sha256"),
		ContentType: contentType,
		UpdatedAt:   updatedAt,
	}
}

func userMetadataValue(info minio.ObjectInfo, key string) string {
	for itemKey, value := range info.UserMetadata {
		if strings.EqualFold(itemKey, key) {
			return value
		}
	}
	for itemKey, values := range info.Metadata {
		if strings.EqualFold(itemKey, "x-amz-meta-"+key) && len(values) > 0 {
			return values[0]
		}
	}
	return ""
}
