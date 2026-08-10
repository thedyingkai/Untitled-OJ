package logic

import (
	"archive/zip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	pathpkg "path"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

const (
	maxWorkerLogBytes      = 64 * 1024
	maxWorkerResultBytes   = 8 * 1024 * 1024
	artifactPackageMaxSize = 512 * 1024 * 1024
)

var terminalJudgeStatuses = map[string]bool{
	"ACCEPTED":              true,
	"WRONG_ANSWER":          true,
	"COMPILE_ERROR":         true,
	"RUNTIME_ERROR":         true,
	"TIME_LIMIT_EXCEEDED":   true,
	"MEMORY_LIMIT_EXCEEDED": true,
	"OUTPUT_LIMIT_EXCEEDED": true,
	"SYSTEM_ERROR":          true,
	"CANCELLED":             true,
	"UNSUPPORTED_LANGUAGE":  true,
}

func workerLeaseTTL(svcCtx *svc.ServiceContext) time.Duration {
	seconds := svcCtx.Config.WorkerAuth.LeaseTTLSeconds
	if seconds <= 0 {
		seconds = 60
	}
	return time.Duration(seconds) * time.Second
}

func workerTaskRepo(svcCtx *svc.ServiceContext) svc.WorkerTaskRepository {
	if svcCtx == nil {
		return nil
	}
	if svcCtx.WorkerRepo != nil {
		return svcCtx.WorkerRepo
	}
	if svcCtx.Repo != nil {
		return svcCtx.Repo
	}
	return nil
}

func validateWorkerIdentity(ctx context.Context, workerID string) error {
	claims, managed := middleware.WorkloadClaimsFromContext(ctx)
	if !managed {
		return nil
	}
	if strings.TrimSpace(workerID) != strings.TrimSpace(claims.DeploymentID) {
		return errors.New("worker_id does not match authenticated deployment")
	}
	return nil
}

func workerBasePath(path string) string {
	path = strings.TrimRight(strings.TrimSpace(path), "/")
	if path == "" {
		return "/judge/worker"
	}
	return path
}

func validateWorkerStatus(status string) error {
	if !terminalJudgeStatuses[status] {
		return fmt.Errorf("invalid judge status: %s", status)
	}
	return nil
}

func normalizeWorkerTaskIDs(taskIDs []string) []string {
	seen := make(map[string]struct{}, len(taskIDs))
	normalized := make([]string, 0, len(taskIDs))
	for _, taskID := range taskIDs {
		taskID = strings.TrimSpace(taskID)
		if taskID == "" {
			continue
		}
		if _, ok := seen[taskID]; ok {
			continue
		}
		seen[taskID] = struct{}{}
		normalized = append(normalized, taskID)
	}
	return normalized
}

func taskLeaseToResp(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	r *repository.TaskLeaseView,
) (types.WorkerTaskLease, error) {
	repo := workerTaskRepo(svcCtx)
	if repo == nil {
		return types.WorkerTaskLease{}, errors.New("worker repository is not configured")
	}

	submission, err := repo.GetSubmission(ctx, r.SubmissionID)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	sourcePath := fmt.Sprintf(
		"/judge/worker/artifacts/submissions/%d/source?task_id=%s&worker_id=%s&lease_version=%d",
		r.SubmissionID,
		r.TaskID,
		r.WorkerID,
		r.LeaseVersion,
	)
	source, err := artifactRefForSubmissionSource(
		ctx,
		svcCtx,
		submission.CodePath,
		sourcePath,
		"text/plain; charset=utf-8",
	)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	legacyPackagePath := fmt.Sprintf(
		"/judge/worker/artifacts/problems/%d/package?task_id=%s&worker_id=%s&lease_version=%d",
		r.ProblemID,
		r.TaskID,
		r.WorkerID,
		r.LeaseVersion,
	)
	var problemPackage types.WorkerArtifactRef
	if strings.TrimSpace(submission.ProblemArtifactURI) != "" {
		problemPackage, err = artifactRefForProblemSnapshot(svcCtx, submission, legacyPackagePath)
	} else {
		problem, problemErr := repo.GetProblemMeta(ctx, r.ProblemID)
		if problemErr != nil {
			return types.WorkerTaskLease{}, problemErr
		}
		packageZip, packageErr := ensureProblemPackageZip(problem.ID, problem.PackageDir)
		if packageErr != nil {
			return types.WorkerTaskLease{}, packageErr
		}
		problemPackage, err = artifactRefForFile(packageZip, legacyPackagePath, "application/zip")
	}
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	if _, managed := middleware.WorkloadClaimsFromContext(ctx); managed {
		if source.Binding == "" || problemPackage.Binding == "" {
			return types.WorkerTaskLease{}, errors.New("managed workers require storage-backed artifacts")
		}
		source.Url = ""
		problemPackage.Url = ""
	}

	return types.WorkerTaskLease{
		TaskId:         r.TaskID,
		SubmissionId:   r.SubmissionID,
		ProblemId:      r.ProblemID,
		Language:       r.Language,
		Attempt:        r.Attempt,
		LeaseVersion:   r.LeaseVersion,
		LeaseExpiresAt: r.LeaseExpiresAt.UTC().Format(time.RFC3339Nano),
		Source:         source,
		ProblemPackage: problemPackage,
	}, nil
}

func artifactRefForProblemSnapshot(svcCtx *svc.ServiceContext, submission *repository.SubmissionView, legacyURLPath string) (types.WorkerArtifactRef, error) {
	if submission == nil || strings.TrimSpace(submission.ProblemArtifactSHA256) == "" || submission.ProblemArtifactSizeBytes <= 0 {
		return types.WorkerArtifactRef{}, errors.New("submission problem artifact snapshot is incomplete")
	}
	uri := strings.TrimSpace(submission.ProblemArtifactURI)
	if strings.HasPrefix(uri, "file://") {
		if !svcCtx.Config.WorkloadIdentity.AllowLegacyWorkerToken {
			return types.WorkerArtifactRef{}, errors.New("local problem artifacts are allowed only for the legacy development worker path")
		}
		return artifactRefForFile(strings.TrimPrefix(uri, "file://"), legacyURLPath, "application/zip")
	}
	bucket, key, ok := parseStorageRef(uri)
	if !ok {
		return types.WorkerArtifactRef{}, fmt.Errorf("unsupported problem artifact URI: %s", uri)
	}
	resourceSHA256, err := apiResourceSHA256(submission.ProblemArtifactSHA256)
	if err != nil {
		return types.WorkerArtifactRef{}, fmt.Errorf("invalid problem artifact digest: %w", err)
	}
	storageCfg := svcCtx.Config.Storage
	storageClient := newStorageClient(storageCfg)
	if storageClient.managedErr != nil {
		return types.WorkerArtifactRef{}, storageClient.managedErr
	}
	// A managed task carries only a stable binding name plus a relative object
	// path. Embedding even a Gateway URL would let stale topology leak into the
	// durable task and would bypass a later Binding switch. URL remains solely
	// for the legacy development path.
	var artifactURL string
	if storageClient.managed != nil || !svcCtx.Config.WorkloadIdentity.AllowLegacyWorkerToken {
		artifactURL = ""
	} else if strings.TrimSpace(storageCfg.InternalGatewayEndpoint) != "" {
		artifactURL = "/internal/apis/" + url.PathEscape(firstNonEmpty(storageCfg.GetApiID, "storage.object.get")) + "/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
	} else if endpoint := strings.TrimRight(strings.TrimSpace(storageCfg.ServiceEndpoint), "/"); endpoint != "" {
		artifactURL = endpoint + "/api/storage/objects/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key))
	} else {
		return types.WorkerArtifactRef{}, errors.New("storage endpoint for problem artifact is not configured")
	}
	return types.WorkerArtifactRef{
		Url:          artifactURL,
		Binding:      "storage_get",
		ApiId:        firstNonEmpty(storageCfg.GetApiID, "storage.object.get"),
		RelativePath: "/" + url.PathEscape(bucket) + "/" + url.PathEscape(cleanStorageKey(key)),
		Sha256:       resourceSHA256,
		SizeBytes:    submission.ProblemArtifactSizeBytes,
		ContentType:  "application/zip",
	}, nil
}

func artifactRefForFile(path string, urlPath string, contentType string) (types.WorkerArtifactRef, error) {
	stat, err := os.Stat(path)
	if err != nil {
		return types.WorkerArtifactRef{}, err
	}
	if stat.Size() < 0 || stat.Size() > artifactPackageMaxSize {
		return types.WorkerArtifactRef{}, fmt.Errorf("artifact size is invalid: %s", path)
	}
	digest, err := sha256File(path)
	if err != nil {
		return types.WorkerArtifactRef{}, err
	}
	resourceSHA256, err := apiResourceSHA256(digest)
	if err != nil {
		return types.WorkerArtifactRef{}, err
	}
	return types.WorkerArtifactRef{
		Url:         urlPath,
		Sha256:      resourceSHA256,
		SizeBytes:   stat.Size(),
		ContentType: contentType,
	}, nil
}

func artifactRefForSubmissionSource(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	path string,
	urlPath string,
	contentType string,
) (types.WorkerArtifactRef, error) {
	bucket, key, ok := parseStorageRef(path)
	if !ok {
		if !svcCtx.Config.WorkloadIdentity.AllowLegacyWorkerToken {
			return types.WorkerArtifactRef{}, errors.New("local submission artifacts are allowed only for the legacy development worker path")
		}
		return artifactRefForFile(path, urlPath, contentType)
	}
	client := newStorageClient(svcCtx.Config.Storage)
	if client.managedErr != nil {
		return types.WorkerArtifactRef{}, client.managedErr
	}
	meta, err := client.getMetadata(ctx, bucket, key)
	if err != nil {
		return types.WorkerArtifactRef{}, err
	}
	if meta.SizeBytes < 0 || meta.SizeBytes > artifactPackageMaxSize {
		return types.WorkerArtifactRef{}, fmt.Errorf("artifact size is invalid: %s", path)
	}
	if meta.ContentType != "" {
		contentType = meta.ContentType
	}
	resourceSHA256, err := apiResourceSHA256(meta.SHA256)
	if err != nil {
		return types.WorkerArtifactRef{}, fmt.Errorf("invalid source artifact digest: %w", err)
	}
	if client.managed != nil || !svcCtx.Config.WorkloadIdentity.AllowLegacyWorkerToken {
		urlPath = ""
	} else if strings.TrimSpace(svcCtx.Config.Storage.InternalGatewayEndpoint) != "" {
		urlPath = "/internal/apis/" + firstNonEmpty(svcCtx.Config.Storage.GetApiID, "storage.object.get") + "/" + bucket + "/" + key
	}
	return types.WorkerArtifactRef{
		Url:          urlPath,
		Binding:      "storage_get",
		ApiId:        firstNonEmpty(svcCtx.Config.Storage.GetApiID, "storage.object.get"),
		RelativePath: "/" + bucket + "/" + key,
		Sha256:       resourceSHA256,
		SizeBytes:    meta.SizeBytes,
		ContentType:  contentType,
	}, nil
}

func apiResourceSHA256(value string) (string, error) {
	digest := strings.ToLower(strings.TrimSpace(value))
	digest = strings.TrimPrefix(digest, "sha256:")
	if len(digest) != 64 {
		return "", errors.New("sha256 must contain exactly 64 hexadecimal characters")
	}
	for _, char := range digest {
		if (char < '0' || char > '9') && (char < 'a' || char > 'f') {
			return "", errors.New("sha256 contains a non-hexadecimal character")
		}
	}
	return "sha256:" + digest, nil
}

func ensureProblemPackageZip(problemID int64, packageDir string) (string, error) {
	if strings.TrimSpace(packageDir) == "" {
		return "", errors.New("problem package path is empty")
	}

	root, err := filepath.Abs(packageDir)
	if err != nil {
		return "", err
	}
	stat, err := os.Stat(root)
	if err != nil {
		return "", err
	}
	if !stat.IsDir() {
		return "", fmt.Errorf("problem package is not a directory: %s", root)
	}

	cacheDir := filepath.Join(os.TempDir(), "ojos-artifacts", "problem-packages")
	if err := os.MkdirAll(cacheDir, 0o755); err != nil {
		return "", err
	}

	zipPath := filepath.Join(cacheDir, fmt.Sprintf("%d.zip", problemID))
	rootStat, _ := os.Stat(root)
	zipStat, zipErr := os.Stat(zipPath)
	if zipErr == nil && !zipStat.ModTime().Before(rootStat.ModTime()) {
		return zipPath, nil
	}

	tmpPath := zipPath + ".tmp"
	_ = os.Remove(tmpPath)
	if err := zipDirectory(root, tmpPath); err != nil {
		_ = os.Remove(tmpPath)
		return "", err
	}
	if err := os.Rename(tmpPath, zipPath); err != nil {
		_ = os.Remove(tmpPath)
		return "", err
	}
	return zipPath, nil
}

func zipDirectory(root string, target string) error {
	out, err := os.Create(target)
	if err != nil {
		return err
	}
	defer out.Close()

	zw := zip.NewWriter(out)
	defer zw.Close()

	var total int64
	return filepath.WalkDir(root, func(path string, d os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if d.IsDir() {
			return nil
		}
		info, err := d.Info()
		if err != nil {
			return err
		}
		if info.Size() < 0 {
			return fmt.Errorf("invalid file size: %s", path)
		}
		total += info.Size()
		if total > artifactPackageMaxSize {
			return errors.New("problem package artifact exceeds size limit")
		}

		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		rel = filepath.ToSlash(rel)
		if strings.HasPrefix(rel, "../") || rel == ".." || filepath.IsAbs(rel) {
			return fmt.Errorf("unsafe package path: %s", rel)
		}

		header, err := zip.FileInfoHeader(info)
		if err != nil {
			return err
		}
		header.Name = rel
		header.Method = zip.Deflate

		writer, err := zw.CreateHeader(header)
		if err != nil {
			return err
		}

		in, err := os.Open(path)
		if err != nil {
			return err
		}
		defer in.Close()

		_, err = io.Copy(writer, io.LimitReader(in, info.Size()))
		return err
	})
}

func sha256File(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()

	h := sha256.New()
	if _, err := io.Copy(h, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func writeWorkerResultArtifacts(
	ctx context.Context,
	storage config.StorageConfig,
	submission *repository.SubmissionView,
	req *types.WorkerSubmitResultReq,
) error {
	if submission.ResultPath == "" {
		return errors.New("submission result path is empty")
	}
	if _, _, ok := parseStorageRef(submission.ResultPath); ok {
		return writeWorkerResultArtifactsToStorage(ctx, storage, submission, req)
	}

	result := ResultFile{
		SubmissionID: submission.ID,
		Status:       req.Status,
		Score:        req.Score,
		TimeMS:       req.TimeMs,
		MemoryKB:     req.MemoryKb,
		Cases:        make([]ResultCaseItem, 0, len(req.Cases)),
	}

	resultDir := filepath.Dir(submission.ResultPath)
	if err := os.MkdirAll(resultDir, 0o755); err != nil {
		return err
	}
	for _, c := range req.Cases {
		caseName := fmt.Sprintf("%03d", c.CaseNo)
		caseDir := filepath.Join(resultDir, "cases", caseName)
		if err := os.MkdirAll(caseDir, 0o755); err != nil {
			return err
		}

		stdoutPath, err := writeBoundedLog(caseDir, "stdout.txt", c.Stdout)
		if err != nil {
			return err
		}
		stderrPath, err := writeBoundedLog(caseDir, "stderr.txt", c.Stderr)
		if err != nil {
			return err
		}
		checkerPath, err := writeBoundedLog(caseDir, "checker.log", c.CheckerLog)
		if err != nil {
			return err
		}

		result.Cases = append(result.Cases, ResultCaseItem{
			CaseNo:         c.CaseNo,
			Status:         c.Status,
			Score:          c.Score,
			TimeMS:         c.TimeMs,
			MemoryKB:       c.MemoryKb,
			StdoutPath:     filepath.ToSlash(stdoutPath),
			StderrPath:     filepath.ToSlash(stderrPath),
			CheckerLogPath: filepath.ToSlash(checkerPath),
			Message:        c.Message,
		})
	}

	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return err
	}
	if len(data) > maxWorkerResultBytes {
		return errors.New("worker result json exceeds size limit")
	}
	return os.WriteFile(submission.ResultPath, data, 0o644)
}

func stageWorkerResultArtifacts(
	ctx context.Context,
	storage config.StorageConfig,
	submission *repository.SubmissionView,
	req *types.WorkerSubmitResultReq,
	leaseVersion int,
	payloadSHA256 string,
) (string, error) {
	if submission == nil {
		return "", errors.New("submission is required")
	}
	if leaseVersion <= 0 || len(payloadSHA256) != 64 {
		return "", errors.New("worker result receipt identity is invalid")
	}
	resultPath := versionedWorkerResultPath(submission, leaseVersion, payloadSHA256)
	staged := *submission
	staged.ResultPath = resultPath
	if err := writeWorkerResultArtifacts(ctx, storage, &staged, req); err != nil {
		return "", err
	}
	return resultPath, nil
}

func versionedWorkerResultPath(
	submission *repository.SubmissionView,
	leaseVersion int,
	payloadSHA256 string,
) string {
	if bucket, _, ok := parseStorageRef(submission.ResultPath); ok {
		// Storage Service v1 exposes the object key as one route segment. Keep
		// the canonical reference identical to the key that storageClient sends
		// instead of persisting a slash-delimited name which gets flattened only
		// on the wire and can never be read back.
		return storageRef(bucket, cleanStorageKey(pathpkg.Join(
			"judge-results",
			strconv.FormatInt(submission.ID, 10),
			strconv.Itoa(leaseVersion),
			payloadSHA256,
			"result.json",
		)))
	}
	return filepath.Join(
		filepath.Dir(submission.ResultPath),
		".receipts",
		strconv.Itoa(leaseVersion),
		payloadSHA256,
		"result.json",
	)
}

func writeWorkerResultArtifactsToStorage(
	ctx context.Context,
	storage config.StorageConfig,
	submission *repository.SubmissionView,
	req *types.WorkerSubmitResultReq,
) error {
	bucket, resultKey, ok := parseStorageRef(submission.ResultPath)
	if !ok {
		return errors.New("submission result path is not a storage ref")
	}
	result := ResultFile{
		SubmissionID: submission.ID,
		Status:       req.Status,
		Score:        req.Score,
		TimeMS:       req.TimeMs,
		MemoryKB:     req.MemoryKb,
		Cases:        make([]ResultCaseItem, 0, len(req.Cases)),
	}

	for _, c := range req.Cases {
		caseName := fmt.Sprintf("%03d", c.CaseNo)
		stdoutPath := storageRef(bucket, fmt.Sprintf("%d-cases-%s-stdout.txt", submission.ID, caseName))
		stderrPath := storageRef(bucket, fmt.Sprintf("%d-cases-%s-stderr.txt", submission.ID, caseName))
		checkerPath := storageRef(bucket, fmt.Sprintf("%d-cases-%s-checker.log", submission.ID, caseName))
		cleanResultKey := cleanStorageKey(resultKey)
		if strings.HasPrefix(cleanResultKey, "judge-results-") && strings.HasSuffix(cleanResultKey, "-result.json") {
			casePrefix := strings.TrimSuffix(cleanResultKey, "-result.json") + "-cases-" + caseName
			stdoutPath = storageRef(bucket, casePrefix+"-stdout.txt")
			stderrPath = storageRef(bucket, casePrefix+"-stderr.txt")
			checkerPath = storageRef(bucket, casePrefix+"-checker.log")
		}
		if err := putBoundedStorageLog(ctx, storage, stdoutPath, c.Stdout); err != nil {
			return err
		}
		if err := putBoundedStorageLog(ctx, storage, stderrPath, c.Stderr); err != nil {
			return err
		}
		if err := putBoundedStorageLog(ctx, storage, checkerPath, c.CheckerLog); err != nil {
			return err
		}

		result.Cases = append(result.Cases, ResultCaseItem{
			CaseNo:         c.CaseNo,
			Status:         c.Status,
			Score:          c.Score,
			TimeMS:         c.TimeMs,
			MemoryKB:       c.MemoryKb,
			StdoutPath:     stdoutPath,
			StderrPath:     stderrPath,
			CheckerLogPath: checkerPath,
			Message:        c.Message,
		})
	}

	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return err
	}
	if len(data) > maxWorkerResultBytes {
		return errors.New("worker result json exceeds size limit")
	}
	return putStorageObject(
		ctx,
		storage,
		submission.ResultPath,
		"application/json; charset=utf-8",
		strings.NewReader(string(data)),
	)
}

func putBoundedStorageLog(
	ctx context.Context,
	storage config.StorageConfig,
	path string,
	content string,
) error {
	if len([]byte(content)) > maxWorkerLogBytes {
		content = string([]byte(content)[:maxWorkerLogBytes])
	}
	return putStorageObject(
		ctx,
		storage,
		path,
		"text/plain; charset=utf-8",
		strings.NewReader(content),
	)
}

func writeBoundedLog(dir string, name string, content string) (string, error) {
	path := filepath.Join(dir, name)
	if len([]byte(content)) > maxWorkerLogBytes {
		content = string([]byte(content)[:maxWorkerLogBytes])
	}
	return path, os.WriteFile(path, []byte(content), 0o644)
}
