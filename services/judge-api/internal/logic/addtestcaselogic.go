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

type AddTestCaseLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAddTestCaseLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AddTestCaseLogic {
	return &AddTestCaseLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AddTestCaseLogic) AddTestCase(req *types.AddTestCaseReq) (resp *types.AddTestCaseResp, err error) {
	if req.ProblemId <= 0 {
		return nil, errors.New("problem_id is required")
	}

	if req.Score <= 0 {
		req.Score = 100
	}

	id, err := l.svcCtx.Repo.AddTestCase(
		l.ctx,
		req.ProblemId,
		req.Input,
		req.Output,
		req.Score,
	)
	if err != nil {
		return nil, err
	}

	return &types.AddTestCaseResp{
		TestCaseId: id,
	}, nil
}
