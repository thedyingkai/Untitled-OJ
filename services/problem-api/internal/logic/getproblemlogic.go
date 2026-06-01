// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

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

	if err := permission.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.view",
		permission.Scope{Type: "problem", ID: req.Id},
	); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	return &types.GetProblemResp{
		Problem: convertProblem(*p),
	}, nil
}
