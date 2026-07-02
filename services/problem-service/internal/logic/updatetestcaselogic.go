// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/packagefs"
	problemstorage "ojos-problem-service/internal/storage"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type UpdateTestCaseLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUpdateTestCaseLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdateTestCaseLogic {
	return &UpdateTestCaseLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdateTestCaseLogic) UpdateTestCase(req *types.UpdateTestCaseReq) (resp *types.UpdateTestCaseResp, err error) {
	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.manage.data", req.ProblemId); err != nil {
		return nil, err
	}
	if req.CaseNo <= 0 {
		return nil, errors.New("invalid request")
	}

	if err := validateLimits(req.TimeLimitMs, req.MemoryLimitMb, true); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	result, err := packagefs.UpdateCase(packagefs.UpdateCaseArgs{
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

	return &types.UpdateTestCaseResp{
		CaseNo: result.CaseNo,
	}, nil
}
