// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type RejudgeProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewRejudgeProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *RejudgeProblemLogic {
	return &RejudgeProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *RejudgeProblemLogic) RejudgeProblem(req *types.RejudgeProblemReq) (resp *types.RejudgeProblemResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if _, err := l.svcCtx.Repo.GetProblemMeta(l.ctx, req.Id); err != nil {
		return nil, err
	}

	permissions := l.svcCtx.ActivePermissionChecker()
	if permissions == nil {
		return nil, errors.New("permission checker is not configured")
	}
	if err := permissions.RequireUserPermission(
		l.ctx,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: req.Id},
	); err != nil {
		return nil, err
	}

	ids, err := l.svcCtx.Repo.ResetSubmissionsForProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	enqueued := 0
	for _, submissionID := range ids {
		if err := l.svcCtx.Repo.EnsureTaskForSubmission(l.ctx, submissionID); err != nil {
			return nil, err
		}
		if err := l.publishSubmissionCreated(submissionID); err != nil {
			return nil, err
		}
		enqueued++
	}

	return &types.RejudgeProblemResp{
		ProblemId: req.Id,
		Enqueued:  enqueued,
	}, nil
}

func (l *RejudgeProblemLogic) publishSubmissionCreated(submissionID int64) error {
	return publishJudgeSignal(l.ctx, l.svcCtx, "submission.created", "judge-api-service", submissionID)
}
