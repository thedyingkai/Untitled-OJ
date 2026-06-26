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
	"os"
	"path/filepath"
	"strings"
	"time"

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

func taskLeaseToResp(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	r *repository.TaskLeaseView,
) (types.WorkerTaskLease, error) {
	submission, err := svcCtx.Repo.GetSubmission(ctx, r.SubmissionID)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	problem, err := svcCtx.Repo.GetProblemMeta(ctx, r.ProblemID)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	source, err := artifactRefForFile(
		submission.CodePath,
		fmt.Sprintf(
			"/artifacts/submissions/%d/source?task_id=%s&worker_id=%s&lease_version=%d",
			r.SubmissionID,
			r.TaskID,
			r.WorkerID,
			r.LeaseVersion,
		),
		"text/plain; charset=utf-8",
	)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	packageZip, err := ensureProblemPackageZip(problem.ID, problem.PackageDir)
	if err != nil {
		return types.WorkerTaskLease{}, err
	}

	problemPackage, err := artifactRefForFile(
		packageZip,
		fmt.Sprintf(
			"/artifacts/problems/%d/package?task_id=%s&worker_id=%s&lease_version=%d",
			r.ProblemID,
			r.TaskID,
			r.WorkerID,
			r.LeaseVersion,
		),
		"application/zip",
	)
	if err != nil {
		return types.WorkerTaskLease{}, err
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
	return types.WorkerArtifactRef{
		Url:         urlPath,
		Sha256:      digest,
		SizeBytes:   stat.Size(),
		ContentType: contentType,
	}, nil
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
	submission *repository.SubmissionView,
	req *types.WorkerSubmitResultReq,
) error {
	if submission.ResultPath == "" {
		return errors.New("submission result path is empty")
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

func writeBoundedLog(dir string, name string, content string) (string, error) {
	path := filepath.Join(dir, name)
	if len([]byte(content)) > maxWorkerLogBytes {
		content = string([]byte(content)[:maxWorkerLogBytes])
	}
	return path, os.WriteFile(path, []byte(content), 0o644)
}
