// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
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

	_, err = packagemutation.RunExisting(
		l.ctx,
		l.svcCtx.Repo,
		l.svcCtx.Config.Storage,
		req.ProblemId,
		func(_ *repository.Problem, stagingDir string) (packagemutation.Change, error) {
			deleted, changed, err := packagefs.DeleteCase(stagingDir, req.CaseNo)
			return packagemutation.Change{Files: changed, DeletedLogicalPaths: deleted}, err
		},
		func(txRepo *repository.Repository, _ *repository.Problem, change packagemutation.Change) error {
			if err := txRepo.DeleteProblemFiles(l.ctx, req.ProblemId, change.DeletedLogicalPaths); err != nil {
				return err
			}
			return txRepo.UpsertProblemFiles(l.ctx, req.ProblemId, change.Files)
		},
	)
	if err != nil {
		return nil, err
	}

	return &types.DeleteTestCaseResp{
		Deleted: true,
	}, nil
}
