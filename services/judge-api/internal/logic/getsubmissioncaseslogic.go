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

type GetSubmissionCasesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetSubmissionCasesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetSubmissionCasesLogic {
	return &GetSubmissionCasesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetSubmissionCasesLogic) GetSubmissionCases(req *types.GetSubmissionCasesReq) (resp *types.GetSubmissionCasesResp, err error) {
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

	cases, err := readResultCases(submission.ResultPath)
	if err != nil {
		return nil, err
	}

	return &types.GetSubmissionCasesResp{
		Cases: cases,
	}, nil
}
