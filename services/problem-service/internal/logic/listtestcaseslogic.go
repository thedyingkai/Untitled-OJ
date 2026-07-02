// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListTestCasesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListTestCasesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListTestCasesLogic {
	return &ListTestCasesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListTestCasesLogic) ListTestCases(req *types.ListTestCasesReq) (resp *types.ListTestCasesResp, err error) {
	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.testdata.read", req.ProblemId); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	cases, err := packagefs.ListCases(p.PackageDir)
	if err != nil {
		return nil, err
	}

	items := make([]types.TestCaseItem, 0, len(cases))
	for _, c := range cases {
		items = append(items, convertCase(c))
	}

	return &types.ListTestCasesResp{
		Cases: items,
	}, nil
}
