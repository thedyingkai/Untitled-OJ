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
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	canViewPrivate, err := l.svcCtx.Repo.CanViewPrivateProblems(l.ctx, user.UserID)
	if err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblemVisibleToUser(l.ctx, req.Id, user.UserID, canViewPrivate)
	if err != nil {
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
