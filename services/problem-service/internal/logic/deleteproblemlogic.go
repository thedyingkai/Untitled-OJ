// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type DeleteProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewDeleteProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *DeleteProblemLogic {
	return &DeleteProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *DeleteProblemLogic) DeleteProblem(req *types.DeleteProblemReq) (resp *types.DeleteProblemResp, err error) {
	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.delete", req.Id); err != nil {
		return nil, err
	}

	if err := packagemutation.RunDelete(
		l.ctx,
		l.svcCtx.Repo,
		l.svcCtx.Config.Storage.ProblemsRoot,
		req.Id,
		func(txRepo *repository.Repository, _ *repository.Problem, expectedAggregateVersion int64) error {
			if err := txRepo.EnqueueProblemDeletedCAS(l.ctx, req.Id, expectedAggregateVersion); err != nil {
				return err
			}
			return txRepo.DeleteProblem(l.ctx, req.Id)
		},
	); err != nil {
		return nil, err
	}

	return &types.DeleteProblemResp{
		Deleted: true,
	}, nil
}
