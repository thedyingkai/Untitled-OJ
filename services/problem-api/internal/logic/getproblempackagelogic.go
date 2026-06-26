// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-problem-api/internal/packagefs"
	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetProblemPackageLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetProblemPackageLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetProblemPackageLogic {
	return &GetProblemPackageLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetProblemPackageLogic) GetProblemPackage(req *types.GetProblemPackageReq) (resp *types.GetProblemPackageResp, err error) {
	if err := requireProblemDataPermission(l.ctx, l.svcCtx, req.ProblemId); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	inspection, err := packagefs.InspectPackage(p.PackageDir)
	if err != nil {
		return nil, err
	}

	return &types.GetProblemPackageResp{
		Package:    convertPackageSummary(inspection.Summary),
		Validation: convertPackageValidation(inspection.Validation),
	}, nil
}
