package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"mime"
	"net/http"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"ojos-storage-service/internal/types"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
)

const maxObjectBytes = 512 * 1024 * 1024

type MinIOObjectStore struct {
	client  *minio.Client
	buckets map[string]struct{}
	now     func() time.Time
	mu      sync.Mutex
}

func NewMinIOObjectStore(options MinIOOptions, buckets []string) (*MinIOObjectStore, error) {
	endpoint := strings.TrimSpace(options.Endpoint)
	if endpoint == "" {
		return nil, fmt.Errorf("minio endpoint is required")
	}
	accessKey := strings.TrimSpace(options.AccessKey)
	secretKey := strings.TrimSpace(options.SecretKey)
	if accessKey == "" || secretKey == "" {
		return nil, fmt.Errorf("minio access key and secret key are required")
	}
	client, err := minio.New(endpoint, &minio.Options{
		Creds:        credentials.NewStaticV4(accessKey, secretKey, ""),
		Secure:       options.UseSSL,
		BucketLookup: minio.BucketLookupPath,
		MaxRetries:   1,
	})
	if err != nil {
		return nil, err
	}
	store := &MinIOObjectStore{
		client:  client,
		buckets: bucketSet(buckets),
		now:     time.Now,
	}
	if err := store.ensureBuckets(context.Background()); err != nil {
		return nil, err
	}
	return store, nil
}

func (s *MinIOObjectStore) Backend() string {
	return "minio"
}

func (s *MinIOObjectStore) BucketNames() []string {
	s.mu.Lock()
	defer s.mu.Unlock()

	names := make([]string, 0, len(s.buckets))
	for bucket := range s.buckets {
		names = append(names, bucket)
	}
	sort.Strings(names)
	return names
}

func (s *MinIOObjectStore) EnsureBucket(bucket string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	created, err := s.ensureBucketLocked(context.Background(), bucket)
	if err != nil {
		return false, err
	}
	return created, nil
}

func (s *MinIOObjectStore) Put(bucket, key, contentType string, body io.Reader) (types.ObjectMetadata, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	key, err := cleanObjectKey(key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if _, err := s.ensureBucketLocked(context.Background(), bucket); err != nil {
		return types.ObjectMetadata{}, err
	}
	data, err := readLimitedObject(body)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if contentType == "" {
		contentType = mime.TypeByExtension(filepath.Ext(key))
	}
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	hash := sha256.Sum256(data)
	sha := hex.EncodeToString(hash[:])
	meta := types.ObjectMetadata{
		Bucket:      bucket,
		Key:         key,
		SizeBytes:   int64(len(data)),
		SHA256:      sha,
		ContentType: contentType,
		UpdatedAt:   s.now().UTC().Format(time.RFC3339),
	}
	_, err = s.client.PutObject(
		context.Background(),
		bucket,
		key,
		bytes.NewReader(data),
		meta.SizeBytes,
		minio.PutObjectOptions{
			ContentType: contentType,
			UserMetadata: map[string]string{
				"ojos-sha256":     sha,
				"ojos-updated-at": meta.UpdatedAt,
			},
		},
	)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	return meta, nil
}

func (s *MinIOObjectStore) Serve(w http.ResponseWriter, r *http.Request, bucket, key string) error {
	key, err := cleanObjectKey(key)
	if err != nil {
		return err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return err
	}
	meta, _ := s.metadata(r.Context(), bucket, key)
	if meta.ContentType != "" {
		w.Header().Set("Content-Type", meta.ContentType)
	}
	if meta.SHA256 != "" {
		w.Header().Set("X-OJOS-Object-Sha256", meta.SHA256)
	}
	if r.Method == http.MethodHead {
		if meta.SizeBytes > 0 {
			w.Header().Set("Content-Length", fmt.Sprintf("%d", meta.SizeBytes))
		}
		return nil
	}
	object, err := s.client.GetObject(r.Context(), bucket, key, minio.GetObjectOptions{})
	if err != nil {
		return err
	}
	defer object.Close()
	_, err = io.Copy(w, object)
	return err
}

func (s *MinIOObjectStore) Delete(bucket, key string) error {
	key, err := cleanObjectKey(key)
	if err != nil {
		return err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return err
	}
	return s.client.RemoveObject(context.Background(), bucket, key, minio.RemoveObjectOptions{})
}

func (s *MinIOObjectStore) Metadata(bucket, key string) (types.ObjectMetadata, error) {
	return s.metadata(context.Background(), bucket, key)
}

func (s *MinIOObjectStore) metadata(ctx context.Context, bucket, key string) (types.ObjectMetadata, error) {
	key, err := cleanObjectKey(key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if err := s.ensureConfiguredBucket(bucket); err != nil {
		return types.ObjectMetadata{}, err
	}
	info, err := s.client.StatObject(ctx, bucket, key, minio.StatObjectOptions{})
	if err != nil {
		return types.ObjectMetadata{}, err
	}
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
		Key:         key,
		SizeBytes:   info.Size,
		SHA256:      userMetadataValue(info, "ojos-sha256"),
		ContentType: contentType,
		UpdatedAt:   updatedAt,
	}, nil
}

func (s *MinIOObjectStore) ensureBuckets(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	for bucket := range s.buckets {
		if _, err := s.ensureBucketLocked(ctx, bucket); err != nil {
			return err
		}
	}
	return nil
}

func (s *MinIOObjectStore) ensureBucketLocked(ctx context.Context, bucket string) (bool, error) {
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
	_, configured := s.buckets[bucket]
	s.buckets[bucket] = struct{}{}
	return !configured || !existed, nil
}

func (s *MinIOObjectStore) ensureConfiguredBucket(bucket string) error {
	if err := validateBucket(bucket); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.buckets[bucket]; !ok {
		return fmt.Errorf("bucket %s is not configured", bucket)
	}
	return nil
}

func readLimitedObject(body io.Reader) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(body, maxObjectBytes+1))
	if err != nil {
		return nil, err
	}
	if len(data) > maxObjectBytes {
		return nil, fmt.Errorf("object exceeds %d bytes", maxObjectBytes)
	}
	return data, nil
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
