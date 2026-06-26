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

type ValidateProblemPackageLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewValidateProblemPackageLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ValidateProblemPackageLogic {
	return &ValidateProblemPackageLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ValidateProblemPackageLogic) ValidateProblemPackage(req *types.ValidateProblemPackageReq) (resp *types.ValidateProblemPackageResp, err error) {
	if err := requireProblemDataPermission(l.ctx, l.svcCtx, req.ProblemId); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.ProblemId)
	if err != nil {
		return nil, err
	}

	validation, err := packagefs.ValidatePackage(p.PackageDir)
	if err != nil {
		return nil, err
	}

	return &types.ValidateProblemPackageResp{
		Validation: convertPackageValidation(*validation),
	}, nil
}
