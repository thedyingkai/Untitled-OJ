// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/submissionfs"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type CreateSubmissionLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

type problemProjectionNotReadyError struct {
	problemID int64
	reason    string
}

func (e problemProjectionNotReadyError) Error() string {
	return fmt.Sprintf("problem %d package projection is not ready: %s", e.problemID, e.reason)
}

func (problemProjectionNotReadyError) HTTPStatus() int { return http.StatusConflict }
func (problemProjectionNotReadyError) ErrorCode() int  { return 40921 }
func (e problemProjectionNotReadyError) PublicMessage() string {
	return e.Error()
}

func NewCreateSubmissionLogic(ctx context.Context, svcCtx *svc.ServiceContext) *CreateSubmissionLogic {
	return &CreateSubmissionLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *CreateSubmissionLogic) CreateSubmission(req *types.CreateSubmissionReq) (resp *types.CreateSubmissionResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	submissions := l.svcCtx.ActiveSubmissionRepo()
	if submissions == nil {
		return nil, errors.New("submission repository is not configured")
	}
	permissions := l.svcCtx.ActivePermissionChecker()
	if permissions == nil {
		return nil, errors.New("permission checker is not configured")
	}

	if err := permissions.RequireUserPermission(
		l.ctx,
		user.UserID,
		"judge.submit",
		sharedperm.SystemScope(),
	); err != nil {
		return nil, err
	}

	if req.ProblemId <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if strings.TrimSpace(req.Code) == "" {
		return nil, errors.New("empty code")
	}

	if int64(len([]byte(req.Code))) > maxCodeBytes(l.svcCtx) {
		return nil, errors.New("code is too large")
	}

	language, err := validateEnabledLanguage(l.svcCtx, req.Language)
	if err != nil {
		return nil, err
	}
	sourceFile := sourceFileForLanguage(l.svcCtx, language)
	if strings.TrimSpace(sourceFile) == "" {
		return nil, errors.New("language source_file is not configured")
	}

	problem, err := submissions.GetProblemMeta(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	if problem.Deleted || problem.Status == "archived" {
		return nil, errors.New("problem is archived")
	}

	if problem.Visibility != "public" && problem.CreatedBy != user.UserID {
		if err := permissions.RequireUserPermission(
			l.ctx,
			user.UserID,
			"problem.view",
			sharedperm.Scope{Type: "problem", ID: req.ProblemId},
		); err != nil {
			return nil, err
		}
	}

	if err := ensureSubmissionProblemProjection(l.svcCtx, problem); err != nil {
		return nil, err
	}

	submissionID, err := submissions.CreateSubmission(
		l.ctx,
		req.ProblemId,
		user.UserID,
		language,
	)
	if err != nil {
		if errors.Is(err, repository.ErrProblemProjectionNotReady) {
			return nil, problemProjectionNotReadyError{
				problemID: req.ProblemId,
				reason:    "projection changed during submission creation; retry after backfill/reconcile completes",
			}
		}
		return nil, err
	}

	var codePath, codeSha256, resultPath string
	if storageEnabled(l.svcCtx.Config.Storage) {
		stored, err := storeSubmissionSource(
			l.ctx,
			l.svcCtx.Config.Storage,
			submissionID,
			sourceFile,
			req.Code,
		)
		if err != nil {
			_ = submissions.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
			return nil, err
		}
		codePath = stored.CodePath
		codeSha256 = stored.CodeSha256
		resultPath = stored.ResultPath
	} else {
		// Local files are an unmanaged development compatibility path. Managed
		// deployments write directly through the storage ApiBindings, which keeps
		// the runtime compatible with a read-only root filesystem.
		files, err := submissionfs.CreateSubmissionFiles(submissionfs.CreateSubmissionFilesArgs{
			Root:         l.svcCtx.Config.Storage.SubmissionsRoot,
			SubmissionID: submissionID,
			Language:     language,
			SourceFile:   sourceFile,
			Code:         req.Code,
		})
		if err != nil {
			_ = submissions.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
			return nil, err
		}
		codePath = files.CodePath
		codeSha256 = files.CodeSha256
		resultPath = files.ResultPath
	}

	if err := submissions.UpdateSubmissionSource(
		l.ctx,
		submissionID,
		codePath,
		codeSha256,
		resultPath,
	); err != nil {
		_ = submissions.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	if err := submissions.EnsureTaskForSubmission(l.ctx, submissionID); err != nil {
		_ = submissions.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	// The PostgreSQL task above is the durable work record. Redis only wakes a
	// worker sooner; a failed signal must not corrupt the submission state or
	// turn a successful create into an API failure.
	notifyJudgeTaskAvailable(l.ctx, l.svcCtx, "submission.created", "judge-api-service", submissionID)

	return &types.CreateSubmissionResp{
		SubmissionId: submissionID,
		Status:       "PENDING",
	}, nil
}

func ensureSubmissionProblemProjection(svcCtx *svc.ServiceContext, problem *repository.ProblemMeta) error {
	if problem == nil {
		return problemProjectionNotReadyError{reason: "problem metadata is missing"}
	}
	if problem.HasManagedPackageArtifact() {
		return nil
	}
	allowLegacy := svcCtx != nil && svcCtx.Config.ProblemProjection.AllowLegacyPackageDir
	if allowLegacy && !problem.HasAnyProjectionArtifactState() && strings.TrimSpace(problem.PackageDir) != "" {
		return nil
	}
	reason := "Problem -> Judge backfill/reconcile has not produced a complete immutable artifact (revision, storage URI, lowercase SHA-256, and positive size)"
	if problem.HasAnyProjectionArtifactState() {
		reason = "the projected artifact is incomplete or invalid; run reconcile before accepting submissions"
	}
	return problemProjectionNotReadyError{problemID: problem.ID, reason: reason}
}
