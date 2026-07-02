// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-problem-service/internal/packagefs"
	problemstorage "ojos-problem-service/internal/storage"
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

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	result, err := packagefs.AddCase(packagefs.AddCaseArgs{
		PackageDir:    p.PackageDir,
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
		return nil, err
	}

	files, err := problemstorage.SyncProblemFiles(l.ctx, l.svcCtx.Config.Storage, req.ProblemId, result.Files)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, req.ProblemId, files); err != nil {
		return nil, err
	}

	return &types.AddTestCaseResp{
		CaseNo: result.CaseNo,
	}, nil
}
