// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

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

	if err := sharedperm.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
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

	problem, err := l.svcCtx.Repo.GetProblemMeta(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	if problem.Status == "archived" {
		return nil, errors.New("problem is archived")
	}

	if problem.Visibility != "public" && problem.CreatedBy != user.UserID {
		if err := sharedperm.RequireUserPermission(
			l.ctx,
			l.svcCtx.DB,
			user.UserID,
			"problem.view",
			sharedperm.Scope{Type: "problem", ID: req.ProblemId},
		); err != nil {
			return nil, err
		}
	}

	if problem.PackageDir == "" {
		return nil, errors.New("problem package is not ready")
	}

	submissionID, err := l.svcCtx.Repo.CreateSubmission(
		l.ctx,
		req.ProblemId,
		user.UserID,
		language,
	)
	if err != nil {
		return nil, err
	}

	files, err := submissionfs.CreateSubmissionFiles(submissionfs.CreateSubmissionFilesArgs{
		Root:         l.svcCtx.Config.Storage.SubmissionsRoot,
		SubmissionID: submissionID,
		Language:     language,
		Code:         req.Code,
	})
	if err != nil {
		_ = l.svcCtx.Repo.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	if err := l.svcCtx.Repo.UpdateSubmissionSource(
		l.ctx,
		submissionID,
		files.CodePath,
		files.CodeSha256,
		files.ResultPath,
	); err != nil {
		_ = l.svcCtx.Repo.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	if err := l.svcCtx.Repo.EnsureTaskForSubmission(l.ctx, submissionID); err != nil {
		_ = l.svcCtx.Repo.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	if err := l.publishSubmissionCreated(submissionID); err != nil {
		_ = l.svcCtx.Repo.MarkSubmissionSystemError(l.ctx, submissionID, err.Error())
		return nil, err
	}

	return &types.CreateSubmissionResp{
		SubmissionId: submissionID,
		Status:       "PENDING",
	}, nil
}

const judgeSubmissionStream = "ojos:judge:submissions"

func (l *CreateSubmissionLogic) publishSubmissionCreated(submissionID int64) error {
	return publishJudgeSignal(l.ctx, l.svcCtx, "submission.created", "judge-api-service", submissionID)
}
