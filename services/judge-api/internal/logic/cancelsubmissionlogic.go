// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type CancelSubmissionLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewCancelSubmissionLogic(ctx context.Context, svcCtx *svc.ServiceContext) *CancelSubmissionLogic {
	return &CancelSubmissionLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *CancelSubmissionLogic) CancelSubmission(req *types.CancelSubmissionReq) (resp *types.CancelSubmissionResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid submission id")
	}

	submission, err := l.svcCtx.Repo.GetSubmission(l.ctx, req.Id)
	if err != nil {
		if errors.Is(err, repository.ErrSubmissionNotFound) {
			return nil, errors.New("submission not found")
		}
		return nil, err
	}

	if err := sharedperm.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.manage.data",
		sharedperm.Scope{Type: "problem", ID: submission.ProblemID},
	); err != nil {
		return nil, err
	}

	reason := strings.TrimSpace(req.Reason)
	if reason == "" {
		reason = "cancelled by problem manager"
	}

	if err := l.svcCtx.Repo.CancelSubmission(l.ctx, req.Id, user.UserID, reason); err != nil {
		return nil, err
	}

	return &types.CancelSubmissionResp{
		SubmissionId: req.Id,
		Status:       "CANCELLED",
	}, nil
}
