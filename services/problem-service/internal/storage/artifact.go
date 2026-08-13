package storage

import (
	"archive/zip"
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
	"path/filepath"
	"sort"
	"strings"
	"time"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/config"
	"ojos-shared/servicecontext"
)

const maxPackageArtifactBytes int64 = 512 * 1024 * 1024

const artifactBuildDirectory = ".ojos-artifact-builds"

var deterministicZipTime = time.Date(1980, time.January, 1, 0, 0, 0, 0, time.UTC)

// ArtifactIntentRegistrar durably records a content-addressed object before the
// first remote byte is uploaded. The matching Problem transaction removes the
// intent only after it has written the immutable revision and outbox snapshot.
// A process crash at any point therefore leaves either a committed reference or
// a recoverable orphan intent, never an untracked object.
type ArtifactIntentRegistrar interface {
	RegisterArtifactUploadIntent(context.Context, problemv1.ArtifactRef) error
	MarkArtifactUploadCompleted(context.Context, problemv1.ArtifactRef) error
}

// PublishPackageArtifactTracked is the production publication path. Remote
// storage uploads are preceded by a separately committed Problem-owned intent;
// local development artifacts do not need a remote-object GC intent.
func PublishPackageArtifactTracked(ctx context.Context, cfg config.StorageConfig, problemID int64, packageDir string, intents ArtifactIntentRegistrar) (problemv1.ArtifactRef, error) {
	managed, err := loadManagedStorageClient()
	if err != nil {
		return problemv1.ArtifactRef{}, err
	}
	if managed != nil {
		defer managed.close()
	}
	problemsRoot := strings.TrimSpace(cfg.ProblemsRoot)
	if problemsRoot == "" {
		// Unmanaged development historically supplied only a package directory.
		// Managed service startup always sets ProblemsRoot to the signed RETAIN
		// volume target, so production package builds never fall back to the
		// container root filesystem.
		if managed != nil {
			return problemv1.ArtifactRef{}, errors.New("managed problem artifact publication requires ProblemsRoot")
		}
		problemsRoot = packageDir
	}
	zipPath, digest, size, err := BuildDeterministicPackageArtifact(problemsRoot, packageDir)
	if err != nil {
		return problemv1.ArtifactRef{}, err
	}
	defer os.Remove(zipPath)

	key := "package-sha256-" + digest + ".zip"
	if managed == nil && strings.TrimSpace(cfg.InternalGatewayEndpoint) == "" && strings.TrimSpace(cfg.ServiceEndpoint) == "" {
		return persistLocalArtifact(cfg.ProblemsRoot, zipPath, key, digest, size)
	}
	artifact := problemv1.ArtifactRef{
		URI:         "storage://" + bucket(cfg) + "/" + key,
		SHA256:      digest,
		SizeBytes:   size,
		ContentType: "application/zip",
	}
	if intents == nil {
		return problemv1.ArtifactRef{}, errors.New("remote problem package publication requires a durable upload-intent registrar")
	}
	if err := intents.RegisterArtifactUploadIntent(ctx, artifact); err != nil {
		return problemv1.ArtifactRef{}, fmt.Errorf("register problem artifact upload intent: %w", err)
	}
	meta, err := putObjectMetadata(ctx, managed, cfg, key, "application/zip", zipPath, digest, size)
	if err != nil {
		return problemv1.ArtifactRef{}, err
	}
	if !strings.EqualFold(strings.TrimSpace(meta.SHA256), digest) {
		return problemv1.ArtifactRef{}, fmt.Errorf("storage package digest mismatch: expected %s, got %s", digest, meta.SHA256)
	}
	if meta.SizeBytes != size {
		return problemv1.ArtifactRef{}, fmt.Errorf("storage package size mismatch: expected %d, got %d", size, meta.SizeBytes)
	}
	if err := intents.MarkArtifactUploadCompleted(ctx, artifact); err != nil {
		return problemv1.ArtifactRef{}, fmt.Errorf("mark problem artifact upload completed: %w", err)
	}
	return artifact, nil
}

// PublishPackageArtifact is retained for unmanaged/local compatibility and
// tests. It deliberately refuses remote storage because that would recreate
// the untracked-upload failure window closed by PublishPackageArtifactTracked.
func PublishPackageArtifact(ctx context.Context, cfg config.StorageConfig, problemID int64, packageDir string) (problemv1.ArtifactRef, error) {
	return PublishPackageArtifactTracked(ctx, cfg, problemID, packageDir, nil)
}

// BuildDeterministicPackageArtifact writes its temporary ZIP into a reserved
// directory below problemsRoot. The managed runtime mounts that root from the
// signed RETAIN volume, allowing the container root filesystem (including
// /tmp) to remain read-only. The caller owns and must remove a successful
// return path; every error path removes it before returning.
func BuildDeterministicPackageArtifact(problemsRoot string, packageDir string) (string, string, int64, error) {
	root, err := filepath.Abs(strings.TrimSpace(packageDir))
	if err != nil {
		return "", "", 0, err
	}
	volumeRoot, err := filepath.Abs(strings.TrimSpace(problemsRoot))
	if err != nil {
		return "", "", 0, err
	}
	if strings.TrimSpace(problemsRoot) == "" {
		return "", "", 0, errors.New("problem artifact build root is required")
	}
	contained, err := filepath.Rel(volumeRoot, root)
	if err != nil || contained == ".." || strings.HasPrefix(contained, ".."+string(filepath.Separator)) || filepath.IsAbs(contained) {
		return "", "", 0, fmt.Errorf("problem package is outside the managed problems root: %s", root)
	}
	buildRoot := filepath.Join(volumeRoot, artifactBuildDirectory)
	stat, err := os.Lstat(root)
	if err != nil {
		return "", "", 0, err
	}
	if !stat.IsDir() || stat.Mode()&os.ModeSymlink != 0 {
		return "", "", 0, fmt.Errorf("problem package is not a real directory: %s", root)
	}

	var files []string
	var total int64
	err = filepath.WalkDir(root, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if sameFilesystemPath(path, buildRoot) {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return fmt.Errorf("problem artifact build path is not a directory: %s", path)
		}
		if entry.IsDir() {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
			return fmt.Errorf("unsupported package entry: %s", path)
		}
		total += info.Size()
		if total > maxPackageArtifactBytes {
			return errors.New("problem package artifact exceeds size limit")
		}
		files = append(files, path)
		return nil
	})
	if err != nil {
		return "", "", 0, err
	}
	sort.Slice(files, func(i, j int) bool {
		left, _ := filepath.Rel(root, files[i])
		right, _ := filepath.Rel(root, files[j])
		return filepath.ToSlash(left) < filepath.ToSlash(right)
	})

	if err := ensureArtifactBuildDirectory(buildRoot); err != nil {
		return "", "", 0, err
	}
	tmp, err := os.CreateTemp(buildRoot, "ojos-problem-package-*.zip")
	if err != nil {
		return "", "", 0, err
	}
	tmpPath := tmp.Name()
	cleanup := func(e error) (string, string, int64, error) {
		_ = tmp.Close()
		_ = os.Remove(tmpPath)
		return "", "", 0, e
	}
	if err := tmp.Chmod(0o600); err != nil {
		return cleanup(err)
	}

	zw := zip.NewWriter(tmp)
	for _, path := range files {
		rel, err := filepath.Rel(root, path)
		if err != nil {
			_ = zw.Close()
			return cleanup(err)
		}
		rel = filepath.ToSlash(rel)
		if strings.HasPrefix(rel, "../") || rel == ".." || filepath.IsAbs(rel) {
			_ = zw.Close()
			return cleanup(fmt.Errorf("unsafe package entry: %s", rel))
		}
		header := &zip.FileHeader{Name: rel, Method: zip.Deflate}
		header.SetModTime(deterministicZipTime)
		header.SetMode(0o644)
		writer, err := zw.CreateHeader(header)
		if err != nil {
			_ = zw.Close()
			return cleanup(err)
		}
		in, err := os.Open(path)
		if err != nil {
			_ = zw.Close()
			return cleanup(err)
		}
		_, copyErr := io.Copy(writer, in)
		closeErr := in.Close()
		if copyErr != nil {
			_ = zw.Close()
			return cleanup(copyErr)
		}
		if closeErr != nil {
			_ = zw.Close()
			return cleanup(closeErr)
		}
	}
	if err := zw.Close(); err != nil {
		return cleanup(err)
	}
	if err := tmp.Close(); err != nil {
		return cleanup(err)
	}
	file, err := os.Open(tmpPath)
	if err != nil {
		return cleanup(err)
	}
	hasher := sha256.New()
	size, err := io.Copy(hasher, file)
	closeErr := file.Close()
	if err != nil {
		return cleanup(err)
	}
	if closeErr != nil {
		return cleanup(closeErr)
	}
	if size > maxPackageArtifactBytes {
		return cleanup(errors.New("problem package artifact exceeds size limit"))
	}
	return tmpPath, hex.EncodeToString(hasher.Sum(nil)), size, nil
}

func ensureArtifactBuildDirectory(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("problem artifact build path is not a private directory: %s", path)
	}
	return os.Chmod(path, 0o700)
}

func sameFilesystemPath(left string, right string) bool {
	left = filepath.Clean(left)
	right = filepath.Clean(right)
	if filepath.Separator == '\\' {
		return strings.EqualFold(left, right)
	}
	return left == right
}

type objectMetadata struct {
	SizeBytes int64  `json:"size_bytes"`
	SHA256    string `json:"sha256"`
}

func putObjectMetadata(ctx context.Context, managed *managedStorageClient, cfg config.StorageConfig, key, contentType, filePath, digest string, size int64) (objectMetadata, error) {
	file, err := os.Open(filePath)
	if err != nil {
		return objectMetadata{}, err
	}
	defer file.Close()
	headers := http.Header{
		"Content-Type":          []string{contentType},
		"X-OJOS-Content-Sha256": []string{digest},
		"If-None-Match":         []string{"*"},
	}
	var req *http.Request
	var client *http.Client
	if managed != nil {
		snapshot, managedClient, managedErr := managed.snapshot(ctx)
		if managedErr != nil {
			return objectMetadata{}, managedErr
		}
		relativePath := "/" + url.PathEscape(bucket(cfg)) + "/" + url.PathEscape(key)
		req, err = snapshot.NewRequestWithOptions(ctx, storagePutBinding, http.MethodPut, relativePath, file, servicecontext.RequestOptions{Headers: headers, ContentLength: size})
		client = managedClient
	} else {
		target, legacyHeaders := putTarget(cfg, key)
		req, err = http.NewRequestWithContext(ctx, http.MethodPut, target, file)
		if err == nil {
			req.ContentLength = size
			req.Header = headers
			for header, value := range legacyHeaders {
				if strings.TrimSpace(value) != "" {
					req.Header.Set(header, value)
				}
			}
		}
		client = &http.Client{Timeout: 10 * time.Minute}
	}
	if err != nil {
		return objectMetadata{}, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return objectMetadata{}, fmt.Errorf("put immutable problem package failed: %w", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
	if err != nil {
		return objectMetadata{}, err
	}
	if resp.StatusCode == http.StatusPreconditionFailed {
		return verifyExistingObject(ctx, managed, client, cfg, key, digest, size)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return objectMetadata{}, fmt.Errorf("put immutable problem package returned %s: %s", resp.Status, strings.TrimSpace(string(body)))
	}
	var meta objectMetadata
	if err := json.Unmarshal(body, &meta); err != nil {
		return objectMetadata{}, fmt.Errorf("decode immutable problem package metadata: %w", err)
	}
	return meta, nil
}

func verifyExistingObject(ctx context.Context, managed *managedStorageClient, client *http.Client, cfg config.StorageConfig, key, digest string, size int64) (objectMetadata, error) {
	var req *http.Request
	var err error
	if managed != nil {
		snapshot, managedClient, managedErr := managed.snapshot(ctx)
		if managedErr != nil {
			return objectMetadata{}, managedErr
		}
		relativePath := "/" + url.PathEscape(bucket(cfg)) + "/" + url.PathEscape(key)
		req, err = snapshot.NewRequest(ctx, storageHeadBinding, http.MethodHead, relativePath, nil)
		client = managedClient
	} else {
		target, headers := headTarget(cfg, key)
		req, err = http.NewRequestWithContext(ctx, http.MethodHead, target, nil)
		if err == nil {
			for header, value := range headers {
				if strings.TrimSpace(value) != "" {
					req.Header.Set(header, value)
				}
			}
		}
	}
	if err != nil {
		return objectMetadata{}, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return objectMetadata{}, fmt.Errorf("head immutable problem package failed: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return objectMetadata{}, fmt.Errorf("head immutable problem package returned %s: %s", resp.Status, strings.TrimSpace(string(body)))
	}
	actualDigest := strings.ToLower(strings.TrimSpace(resp.Header.Get("X-OJOS-Object-Sha256")))
	if actualDigest != strings.ToLower(strings.TrimSpace(digest)) {
		return objectMetadata{}, fmt.Errorf("immutable problem package collision: expected sha256 %s, got %s", digest, actualDigest)
	}
	if resp.ContentLength != size {
		return objectMetadata{}, fmt.Errorf("immutable problem package collision: expected size %d, got %d", size, resp.ContentLength)
	}
	return objectMetadata{SHA256: actualDigest, SizeBytes: resp.ContentLength}, nil
}

func persistLocalArtifact(root, source, key, digest string, size int64) (problemv1.ArtifactRef, error) {
	root = strings.TrimSpace(root)
	if root == "" {
		root = filepath.Dir(source)
	}
	dir := filepath.Join(root, ".artifacts")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return problemv1.ArtifactRef{}, err
	}
	target := filepath.Join(dir, key)
	if _, err := os.Stat(target); err == nil {
		existingDigest, existingSize, err := digestFile(target)
		if err != nil {
			return problemv1.ArtifactRef{}, err
		}
		if existingDigest != digest || existingSize != size {
			return problemv1.ArtifactRef{}, fmt.Errorf("immutable local artifact collision: %s", target)
		}
	} else if !os.IsNotExist(err) {
		return problemv1.ArtifactRef{}, err
	} else {
		in, err := os.Open(source)
		if err != nil {
			return problemv1.ArtifactRef{}, err
		}
		defer in.Close()
		out, err := os.CreateTemp(dir, ".ojos-problem-artifact-*.tmp")
		if err != nil {
			return problemv1.ArtifactRef{}, err
		}
		tmp := out.Name()
		copied, copyErr := io.Copy(out, in)
		closeErr := out.Close()
		if copyErr != nil || closeErr != nil || copied != size {
			_ = os.Remove(tmp)
			if copyErr != nil {
				return problemv1.ArtifactRef{}, copyErr
			}
			if closeErr != nil {
				return problemv1.ArtifactRef{}, closeErr
			}
			return problemv1.ArtifactRef{}, fmt.Errorf("local artifact size changed: expected %d, got %d", size, copied)
		}
		if err := os.Rename(tmp, target); err != nil {
			_ = os.Remove(tmp)
			return problemv1.ArtifactRef{}, err
		}
	}
	abs, err := filepath.Abs(target)
	if err != nil {
		return problemv1.ArtifactRef{}, err
	}
	return problemv1.ArtifactRef{
		URI:         "file://" + filepath.ToSlash(abs),
		SHA256:      digest,
		SizeBytes:   size,
		ContentType: "application/zip",
	}, nil
}

func digestFile(path string) (string, int64, error) {
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

func sha256Hex(data []byte) string {
	hash := sha256.Sum256(data)
	return hex.EncodeToString(hash[:])
}
