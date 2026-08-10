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
	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.testdata.write", req.ProblemId); err != nil {
		return nil, err
	}
	if err := validateLimits(req.TimeLimitMs, req.MemoryLimitMb, true); err != nil {
		return nil, err
	}

	change, err := packagemutation.RunExisting(
		l.ctx,
		l.svcCtx.Repo,
		l.svcCtx.Config.Storage,
		req.ProblemId,
		func(_ *repository.Problem, stagingDir string) (packagemutation.Change, error) {
			result, err := packagefs.AddCase(packagefs.AddCaseArgs{
				PackageDir:    stagingDir,
				CaseNo:        req.CaseNo,
				Input:         req.Input,
				Answer:        req.Answer,
				Score:         req.Score,
				Group:         req.Group,
				Sample:        req.Sample,
				Hidden:        req.Hidden,
				TimeLimitMs:   req.TimeLimitMs,
				MemoryLimitMb: req.MemoryLimitMb,
			})
			if err != nil {
				return packagemutation.Change{}, err
			}
			return packagemutation.Change{Files: result.Files, Value: result.CaseNo}, nil
		},
		func(txRepo *repository.Repository, _ *repository.Problem, change packagemutation.Change) error {
			return txRepo.UpsertProblemFiles(l.ctx, req.ProblemId, change.Files)
		},
	)
	if err != nil {
		return nil, err
	}
	caseNo, ok := change.Value.(int)
	if !ok {
		return nil, errors.New("invalid add testcase mutation result")
	}

	return &types.AddTestCaseResp{
		CaseNo: caseNo,
	}, nil
}
