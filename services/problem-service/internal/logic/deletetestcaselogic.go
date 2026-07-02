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

type DeleteTestCaseLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewDeleteTestCaseLogic(ctx context.Context, svcCtx *svc.ServiceContext) *DeleteTestCaseLogic {
	return &DeleteTestCaseLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *DeleteTestCaseLogic) DeleteTestCase(req *types.DeleteTestCaseReq) (resp *types.DeleteTestCaseResp, err error) {
	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.testdata.write", req.ProblemId); err != nil {
		return nil, err
	}
	if req.CaseNo <= 0 {
		return nil, errors.New("invalid request")
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	deleted, changed, err := packagefs.DeleteCase(p.PackageDir, req.CaseNo)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.DeleteProblemFiles(l.ctx, req.ProblemId, deleted); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, req.ProblemId, changed); err != nil {
		return nil, err
	}

	return &types.DeleteTestCaseResp{
		Deleted: true,
	}, nil
}
