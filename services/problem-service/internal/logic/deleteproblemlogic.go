// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/packagefs"
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

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.DeleteProblem(l.ctx, req.Id); err != nil {
		return nil, err
	}

	if err := packagefs.DeletePackageDir(
		l.svcCtx.Config.Storage.ProblemsRoot,
		p.PackageDir,
	); err != nil {
		return nil, err
	}

	return &types.DeleteProblemResp{
		Deleted: true,
	}, nil
}
