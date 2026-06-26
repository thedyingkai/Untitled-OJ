// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-api/internal/packagefs"
	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

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
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.ProblemId <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if err := permission.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.manage.data",
		permission.Scope{Type: "problem", ID: req.ProblemId},
	); err != nil {
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

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, req.ProblemId, result.Files); err != nil {
		return nil, err
	}

	return &types.AddTestCaseResp{
		CaseNo: result.CaseNo,
	}, nil
}
