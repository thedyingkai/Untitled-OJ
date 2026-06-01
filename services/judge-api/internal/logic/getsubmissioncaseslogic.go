// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

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

	cases, err := l.svcCtx.Repo.GetSubmissionCases(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	items := make([]types.SubmissionCaseItem, 0, len(cases))

	for _, c := range cases {
		items = append(items, types.SubmissionCaseItem{
			Id:           c.ID,
			SubmissionId: c.SubmissionID,
			TestCaseId:   c.TestCaseID,
			Status:       c.Status,
			TimeMs:       c.TimeMS,
			MemoryKb:     c.MemoryKB,
			Message:      c.Message,
		})
	}

	return &types.GetSubmissionCasesResp{
		Cases: items,
	}, nil
}
