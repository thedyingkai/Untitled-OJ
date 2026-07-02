// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetProblemLogic {
	return &GetProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetProblemLogic) GetProblem(req *types.GetProblemReq) (resp *types.GetProblemResp, err error) {
	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}
	requiredPermission := "problem.view"
	if strings.TrimSpace(p.Visibility) != "public" {
		requiredPermission = "problem.view.private"
	}
	if _, err := requireProblemPermission(l.ctx, l.svcCtx, requiredPermission, req.Id); err != nil {
		return nil, err
	}

	item := convertProblem(*p)

	samples, err := packagefs.ReadSamples(p.PackageDir)
	if err != nil {
		return nil, err
	}
	item.Samples = convertSamples(samples)

	return &types.GetProblemResp{
		Problem: item,
	}, nil
}
