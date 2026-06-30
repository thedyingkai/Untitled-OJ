// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

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
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.ProblemId <= 0 || req.CaseNo <= 0 {
		return nil, errors.New("invalid request")
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
