// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetSubmissionLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetSubmissionLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetSubmissionLogic {
	return &GetSubmissionLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetSubmissionLogic) GetSubmission(req *types.GetSubmissionReq) (resp *types.GetSubmissionResp, err error) {
	if req.Id <= 0 {
		return nil, errors.New("invalid submission id")
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("submission repository is not configured")
	}

	submission, err := repo.GetSubmission(l.ctx, req.Id)
	if err != nil {
		if errors.Is(err, repository.ErrSubmissionNotFound) {
			return nil, errors.New("submission not found")
		}
		return nil, err
	}

	if err := requireSubmissionViewPermission(l.ctx, l.svcCtx, submission); err != nil {
		return nil, err
	}

	result := convertSubmission(submission)
	return &result, nil
}
