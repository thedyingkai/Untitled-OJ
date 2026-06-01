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

type CreateProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewCreateProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *CreateProblemLogic {
	return &CreateProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *CreateProblemLogic) CreateProblem(req *types.CreateProblemReq) (resp *types.CreateProblemResp, err error) {
	if req.Title == "" {
		return nil, errors.New("title is required")
	}

	if req.TimeLimitMs <= 0 {
		req.TimeLimitMs = 1000
	}

	if req.MemoryLimitMb <= 0 {
		req.MemoryLimitMb = 256
	}

	id, err := l.svcCtx.Repo.CreateProblem(
		l.ctx,
		req.Title,
		req.TimeLimitMs,
		req.MemoryLimitMb,
	)
	if err != nil {
		return nil, err
	}

	return &types.CreateProblemResp{
		ProblemId: id,
	}, nil
}
