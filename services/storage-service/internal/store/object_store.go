package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"ojos-shared/storagecontract"
	"ojos-storage-service/internal/types"
)

type ObjectStore struct {
	root    string
	buckets map[string]struct{}
	now     func() time.Time
	mu      sync.Mutex
}

var safeBucket = regexp.MustCompile(`^[a-z0-9][a-z0-9.-]{1,62}$`)

const maxObjectBytes int64 = 512 * 1024 * 1024

var (
	ErrObjectNotFound     = errors.New("object not found")
	ErrPreconditionFailed = errors.New("object precondition failed")
)

type PutOptions struct {
	ContentType    string
	SizeBytes      int64
	SizeKnown      bool
	ExpectedSHA256 string
	IfAbsent       bool
}

type ObjectPage struct {
	Objects    []types.ObjectMetadata
	NextCursor string
}

type ObjectStorage interface {
	Backend() string
	Ready(context.Context) error
	BucketNames() []string
	EnsureBucket(bucket string) (bool, error)
	Put(ctx context.Context, bucket, key string, options PutOptions, body io.Reader) (types.ObjectMetadata, error)
	List(ctx context.Context, bucket, prefix, cursor string, limit int) (ObjectPage, error)
	Serve(w http.ResponseWriter, r *http.Request, bucket, key string) error
	Delete(bucket, key string) error
	DeleteIfMatches(ctx context.Context, bucket, key, expectedSHA256 string, expectedSize int64) error
	Metadata(bucket, key string) (types.ObjectMetadata, error)
}

type Options struct {
	Backend string
	Root    string
	Buckets []string
	MinIO   MinIOOptions
}

type MinIOOptions struct {
	Endpoint  string
	AccessKey string
	SecretKey string
	UseSSL    bool
}

func NewObjectStorage(options Options) (ObjectStorage, error) {
	switch strings.ToLower(strings.TrimSpace(options.Backend)) {
	case "", "local":
		return NewObjectStore(options.Root, options.Buckets)
	case "minio":
		return NewMinIOObjectStore(options.MinIO, options.Buckets)
	default:
		return nil, fmt.Errorf("unsupported storage backend %q", options.Backend)
	}
}

func NewObjectStore(root string, buckets []string) (*ObjectStore, error) {
	root = strings.TrimSpace(root)
	if root == "" {
		root = "/data/ojos/storage"
	}
	set := make(map[string]struct{}, len(buckets))
	for _, bucket := range buckets {
		if bucket = strings.TrimSpace(bucket); bucket != "" {
			set[bucket] = struct{}{}
		}
	}
	store := &ObjectStore{
		root:    root,
		buckets: set,
		now:     time.Now,
	}
	if err := store.ensureBuckets(); err != nil {
		return nil, err
	}
	return store, nil
}

func (s *ObjectStore) Backend() string {
	return "local"
}

func (s *ObjectStore) Ready(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for bucket := range s.buckets {
		path := s.bucketDir(bucket)
		info, err := os.Stat(path)
		if err != nil || !info.IsDir() {
			return fmt.Errorf("local storage bucket %s is unavailable", bucket)
		}
	}
	return nil
}

func (s *ObjectStore) BucketNames() []string {
	s.mu.Lock()
	defer s.mu.Unlock()

	names := make([]string, 0, len(s.buckets))
	for bucket := range s.buckets {
		names = append(names, bucket)
	}
	sort.Strings(names)
	return names
}

func (s *ObjectStore) EnsureBucket(bucket string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := validateBucket(bucket); err != nil {
		return false, err
	}
	_, existed := s.buckets[bucket]
	s.buckets[bucket] = struct{}{}
	if err := os.MkdirAll(s.bucketDir(bucket), 0o755); err != nil {
		return false, err
	}
	if err := os.MkdirAll(s.metaBucketDir(bucket), 0o755); err != nil {
		return false, err
	}
	return !existed, nil
}

func (s *ObjectStore) Put(ctx context.Context, bucket, key string, options PutOptions, body io.Reader) (types.ObjectMetadata, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := ctx.Err(); err != nil {
		return types.ObjectMetadata{}, err
	}
	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if options.IfAbsent {
		if _, err := os.Stat(objectPath); err == nil {
			return types.ObjectMetadata{}, ErrPreconditionFailed
		} else if !os.IsNotExist(err) {
			return types.ObjectMetadata{}, err
		}
	}
	if options.SizeKnown && (options.SizeBytes < 0 || options.SizeBytes > maxObjectBytes) {
		return types.ObjectMetadata{}, fmt.Errorf("invalid object size %d", options.SizeBytes)
	}
	expectedSHA, err := normalizeExpectedSHA256(options.ExpectedSHA256)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if err := os.MkdirAll(filepath.Dir(objectPath), 0o755); err != nil {
		return types.ObjectMetadata{}, err
	}
	file, err := os.CreateTemp(filepath.Dir(objectPath), ".ojos-object-*.tmp")
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	tmp := file.Name()
	hasher := sha256.New()
	size, copyErr := io.Copy(io.MultiWriter(file, hasher), io.LimitReader(body, maxObjectBytes+1))
	closeErr := file.Close()
	if copyErr != nil {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, copyErr
	}
	if closeErr != nil {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, closeErr
	}
	if size > maxObjectBytes {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, fmt.Errorf("object exceeds %d bytes", maxObjectBytes)
	}
	if options.SizeKnown && size != options.SizeBytes {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, fmt.Errorf("object size mismatch: expected %d, got %d", options.SizeBytes, size)
	}
	actualSHA := hex.EncodeToString(hasher.Sum(nil))
	if expectedSHA != "" && actualSHA != expectedSHA {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, fmt.Errorf("object sha256 mismatch: expected %s, got %s", expectedSHA, actualSHA)
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
		SHA256:      actualSHA,
		ContentType: contentType,
		UpdatedAt:   s.now().UTC().Format(time.RFC3339),
	}
	if err := os.Rename(tmp, objectPath); err != nil {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, err
	}
	if err := s.writeMetadata(meta); err != nil {
		return types.ObjectMetadata{}, err
	}
	return meta, nil
}

func (s *ObjectStore) List(ctx context.Context, bucket, prefix, cursor string, limit int) (ObjectPage, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := ctx.Err(); err != nil {
		return ObjectPage{}, err
	}
	if err := s.ensureBucket(bucket); err != nil {
		return ObjectPage{}, err
	}
	limit = normalizeListLimit(limit)
	prefix = strings.TrimSpace(prefix)
	cursor = strings.TrimSpace(cursor)
	root := s.bucketDir(bucket)
	items := make([]types.ObjectMetadata, 0)
	err := filepath.WalkDir(root, func(objectPath string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("unsupported stored object: %s", objectPath)
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		relative, err := filepath.Rel(root, objectPath)
		if err != nil {
			return err
		}
		key := filepath.ToSlash(relative)
		if !strings.HasPrefix(key, prefix) || (cursor != "" && key <= cursor) {
			return nil
		}
		meta, err := s.metadataForObject(bucket, key, objectPath, info)
		if err != nil {
			return err
		}
		items = append(items, meta)
		return nil
	})
	if err != nil {
		return ObjectPage{}, err
	}
	sort.Slice(items, func(i, j int) bool { return items[i].Key < items[j].Key })
	page := ObjectPage{Objects: items}
	if len(items) > limit {
		page.Objects = items[:limit]
		page.NextCursor = page.Objects[len(page.Objects)-1].Key
	}
	return page, nil
}

func (s *ObjectStore) Serve(w http.ResponseWriter, r *http.Request, bucket, key string) error {
	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return err
	}
	meta, metaErr := s.Metadata(bucket, key)
	if metaErr != nil {
		if os.IsNotExist(metaErr) {
			if r.Method == http.MethodHead {
				w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultObjectNotFound)
			}
			return ErrObjectNotFound
		}
		return metaErr
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
	}
	http.ServeFile(w, r, objectPath)
	return nil
}

func (s *ObjectStore) Delete(bucket, key string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return err
	}
	if err := os.Remove(objectPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	metaPath, err := s.metaPath(bucket, key)
	if err != nil {
		return err
	}
	if err := os.Remove(metaPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

// DeleteIfMatches makes GC deletion conditional on the immutable identity the
// caller observed. The same store mutex is used by Put, so a local provider
// cannot replace an object between verification and removal.
func (s *ObjectStore) DeleteIfMatches(_ context.Context, bucket, key, expectedSHA256 string, expectedSize int64) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	expectedSHA256, err := normalizeExpectedSHA256(expectedSHA256)
	if err != nil || expectedSHA256 == "" || expectedSize < 0 {
		return ErrPreconditionFailed
	}
	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return err
	}
	info, err := os.Stat(objectPath)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	meta, err := s.metadataForObject(bucket, key, objectPath, info)
	if err != nil {
		return err
	}
	if meta.SHA256 != expectedSHA256 || meta.SizeBytes != expectedSize {
		return ErrPreconditionFailed
	}
	if err := os.Remove(objectPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	metaPath, err := s.metaPath(bucket, key)
	if err != nil {
		return err
	}
	if err := os.Remove(metaPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func (s *ObjectStore) Metadata(bucket, key string) (types.ObjectMetadata, error) {
	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	info, err := os.Stat(objectPath)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	return s.metadataForObject(bucket, key, objectPath, info)
}

func (s *ObjectStore) metadataForObject(bucket, key, objectPath string, info os.FileInfo) (types.ObjectMetadata, error) {
	metaPath, err := s.metaPath(bucket, key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	data, err := os.ReadFile(metaPath)
	if err == nil {
		var meta types.ObjectMetadata
		if err := json.Unmarshal(data, &meta); err != nil {
			return types.ObjectMetadata{}, err
		}
		return meta, nil
	}
	if !os.IsNotExist(err) {
		return types.ObjectMetadata{}, err
	}
	digest, size, err := digestObjectFile(objectPath)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	contentType := mime.TypeByExtension(filepath.Ext(key))
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	return types.ObjectMetadata{
		Bucket:      bucket,
		Key:         key,
		SizeBytes:   size,
		SHA256:      digest,
		ContentType: contentType,
		UpdatedAt:   info.ModTime().UTC().Format(time.RFC3339),
	}, nil
}

func (s *ObjectStore) ensureBuckets() error {
	for bucket := range s.buckets {
		if err := validateBucket(bucket); err != nil {
			return err
		}
		if err := os.MkdirAll(s.bucketDir(bucket), 0o755); err != nil {
			return err
		}
		if err := os.MkdirAll(s.metaBucketDir(bucket), 0o755); err != nil {
			return err
		}
	}
	return nil
}

func (s *ObjectStore) writeMetadata(meta types.ObjectMetadata) error {
	metaPath, err := s.metaPath(meta.Bucket, meta.Key)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(metaPath), 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(metaPath, data, 0o644)
}

func (s *ObjectStore) objectPath(bucket, key string) (string, error) {
	key, err := cleanObjectKey(key)
	if err != nil {
		return "", err
	}
	if err := s.ensureBucket(bucket); err != nil {
		return "", err
	}
	return filepath.Join(s.bucketDir(bucket), filepath.FromSlash(key)), nil
}

func (s *ObjectStore) metaPath(bucket, key string) (string, error) {
	key, err := cleanObjectKey(key)
	if err != nil {
		return "", err
	}
	if err := s.ensureBucket(bucket); err != nil {
		return "", err
	}
	return filepath.Join(s.metaBucketDir(bucket), filepath.FromSlash(key)+".json"), nil
}

func (s *ObjectStore) ensureBucket(bucket string) error {
	if err := validateBucket(bucket); err != nil {
		return err
	}
	if _, ok := s.buckets[bucket]; !ok {
		return fmt.Errorf("bucket %s is not configured", bucket)
	}
	return nil
}

func (s *ObjectStore) bucketDir(bucket string) string {
	return filepath.Join(s.root, "objects", bucket)
}

func (s *ObjectStore) metaBucketDir(bucket string) string {
	return filepath.Join(s.root, "metadata", bucket)
}

func cleanObjectKey(key string) (string, error) {
	if strings.Contains(key, "\\") || strings.Contains(key, "\x00") {
		return "", fmt.Errorf("invalid object key")
	}
	cleaned := path.Clean("/" + key)
	cleaned = strings.TrimPrefix(cleaned, "/")
	if cleaned == "." || cleaned == "" || strings.HasPrefix(cleaned, "../") || cleaned == ".." {
		return "", fmt.Errorf("invalid object key")
	}
	return cleaned, nil
}

func validateBucket(bucket string) error {
	if !safeBucket.MatchString(bucket) {
		return fmt.Errorf("invalid bucket")
	}
	return nil
}

func normalizeExpectedSHA256(value string) (string, error) {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return "", nil
	}
	decoded, err := hex.DecodeString(value)
	if err != nil || len(decoded) != sha256.Size {
		return "", fmt.Errorf("invalid expected sha256")
	}
	return value, nil
}

func normalizeListLimit(limit int) int {
	if limit <= 0 {
		return 100
	}
	if limit > 500 {
		return 500
	}
	return limit
}

func digestObjectFile(path string) (string, int64, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", 0, err
	}
	defer file.Close()
	hasher := sha256.New()
	size, err := io.Copy(hasher, file)
	if err != nil {
		return "", 0, err
	}
	return hex.EncodeToString(hasher.Sum(nil)), size, nil
}
