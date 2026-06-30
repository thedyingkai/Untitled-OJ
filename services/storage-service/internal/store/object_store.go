package store

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
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

	"ojos-storage-service/internal/types"
)

type ObjectStore struct {
	root    string
	buckets map[string]struct{}
	now     func() time.Time
	mu      sync.Mutex
}

var safeBucket = regexp.MustCompile(`^[a-z0-9][a-z0-9.-]{1,62}$`)

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

func (s *ObjectStore) BucketNames() []string {
	names := make([]string, 0, len(s.buckets))
	for bucket := range s.buckets {
		names = append(names, bucket)
	}
	sort.Strings(names)
	return names
}

func (s *ObjectStore) Put(bucket, key, contentType string, body io.Reader) (types.ObjectMetadata, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	if err := os.MkdirAll(filepath.Dir(objectPath), 0o755); err != nil {
		return types.ObjectMetadata{}, err
	}
	tmp := objectPath + ".tmp"
	file, err := os.Create(tmp)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	hasher := sha256.New()
	size, copyErr := io.Copy(file, io.TeeReader(io.LimitReader(body, 512*1024*1024), hasher))
	closeErr := file.Close()
	if copyErr != nil {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, copyErr
	}
	if closeErr != nil {
		_ = os.Remove(tmp)
		return types.ObjectMetadata{}, closeErr
	}
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
		SHA256:      hex.EncodeToString(hasher.Sum(nil)),
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

func (s *ObjectStore) Serve(w http.ResponseWriter, r *http.Request, bucket, key string) error {
	objectPath, err := s.objectPath(bucket, key)
	if err != nil {
		return err
	}
	meta, _ := s.Metadata(bucket, key)
	if meta.ContentType != "" {
		w.Header().Set("Content-Type", meta.ContentType)
	}
	if meta.SHA256 != "" {
		w.Header().Set("X-OJOS-Object-Sha256", meta.SHA256)
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

func (s *ObjectStore) Metadata(bucket, key string) (types.ObjectMetadata, error) {
	metaPath, err := s.metaPath(bucket, key)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	data, err := os.ReadFile(metaPath)
	if err != nil {
		return types.ObjectMetadata{}, err
	}
	var meta types.ObjectMetadata
	if err := json.Unmarshal(data, &meta); err != nil {
		return types.ObjectMetadata{}, err
	}
	return meta, nil
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
