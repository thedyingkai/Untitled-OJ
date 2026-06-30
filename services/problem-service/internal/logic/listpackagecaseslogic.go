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

type ListPackageCasesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListPackageCasesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListPackageCasesLogic {
	return &ListPackageCasesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListPackageCasesLogic) ListPackageCases(req *types.ListPackageCasesReq) (resp *types.ListPackageCasesResp, err error) {
	if err := requireProblemDataPermission(l.ctx, l.svcCtx, req.ProblemId); err != nil {
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

	return &types.ListPackageCasesResp{
		Cases: items,
	}, nil
}
