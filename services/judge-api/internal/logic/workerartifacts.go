package logic

import (
	"context"
	"errors"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

func ServeWorkerSubmissionSource(ctx context.Context, svcCtx *svc.ServiceContext, w http.ResponseWriter, r *http.Request, req *types.WorkerArtifactSourceReq) error {
	if req.Id <= 0 || strings.TrimSpace(req.TaskId) == "" || strings.TrimSpace(req.WorkerId) == "" || req.LeaseVersion <= 0 {
		return errors.New("invalid artifact lease")
	}
	if err := validateWorkerIdentity(ctx, req.WorkerId); err != nil {
		return err
	}

	repo := workerTaskRepo(svcCtx)
	if repo == nil {
		return errors.New("worker repository is not configured")
	}

	lease, err := repo.GetTaskForLease(ctx, req.TaskId, req.WorkerId, req.LeaseVersion)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return errors.New("task lease is invalid")
		}
		return err
	}
	if lease.SubmissionID != req.Id || lease.Status != "RUNNING" || !lease.LeaseExpiresAt.After(time.Now()) {
		return errors.New("artifact lease does not match submission")
	}

	submission, err := repo.GetSubmission(ctx, req.Id)
	if err != nil {
		return err
	}
	if _, _, ok := parseStorageRef(submission.CodePath); ok {
		return serveStorageArtifact(
			ctx,
			svcCtx.Config.Storage,
			w,
			submission.CodePath,
			"text/plain; charset=utf-8",
		)
	}
	return serveArtifactFile(w, r, submission.CodePath, "text/plain; charset=utf-8")
}

func ServeWorkerProblemPackage(ctx context.Context, svcCtx *svc.ServiceContext, w http.ResponseWriter, r *http.Request, req *types.WorkerArtifactProblemPackageReq) error {
	if req.Id <= 0 || strings.TrimSpace(req.TaskId) == "" || strings.TrimSpace(req.WorkerId) == "" || req.LeaseVersion <= 0 {
		return errors.New("invalid artifact lease")
	}
	if err := validateWorkerIdentity(ctx, req.WorkerId); err != nil {
		return err
	}

	repo := workerTaskRepo(svcCtx)
	if repo == nil {
		return errors.New("worker repository is not configured")
	}

	lease, err := repo.GetTaskForLease(ctx, req.TaskId, req.WorkerId, req.LeaseVersion)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return errors.New("task lease is invalid")
		}
		return err
	}
	if lease.ProblemID != req.Id || lease.Status != "RUNNING" || !lease.LeaseExpiresAt.After(time.Now()) {
		return errors.New("artifact lease does not match problem")
	}

	problem, err := repo.GetProblemMeta(ctx, req.Id)
	if err != nil {
		return err
	}
	zipPath, err := ensureProblemPackageZip(problem.ID, problem.PackageDir)
	if err != nil {
		return err
	}
	return serveArtifactFile(w, r, zipPath, "application/zip")
}

func serveArtifactFile(w http.ResponseWriter, r *http.Request, path string, contentType string) error {
	stat, err := os.Stat(path)
	if err != nil {
		return err
	}
	if stat.Size() > artifactPackageMaxSize {
		return errors.New("artifact exceeds size limit")
	}
	digest, err := sha256File(path)
	if err != nil {
		return err
	}

	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()

	w.Header().Set("Content-Type", contentType)
	w.Header().Set("Content-Length", strconv.FormatInt(stat.Size(), 10))
	w.Header().Set("X-OJOS-Artifact-Sha256", digest)
	w.Header().Set("X-OJOS-Artifact-Size", strconv.FormatInt(stat.Size(), 10))
	http.ServeContent(w, r, stat.Name(), stat.ModTime(), file)
	return nil
}
