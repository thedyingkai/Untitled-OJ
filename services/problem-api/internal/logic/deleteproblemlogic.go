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

type DeleteProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewDeleteProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *DeleteProblemLogic {
	return &DeleteProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *DeleteProblemLogic) DeleteProblem(req *types.DeleteProblemReq) (resp *types.DeleteProblemResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	isOwner, err := l.svcCtx.Repo.IsProblemOwner(l.ctx, user.UserID, req.Id)
	if err != nil {
		return nil, err
	}

	if !isOwner {
		if err := permission.RequireUserPermission(
			l.ctx,
			l.svcCtx.DB,
			user.UserID,
			"problem.delete",
			permission.Scope{Type: "problem", ID: req.Id},
		); err != nil {
			return nil, errors.New("forbidden: only problem owner or problem manager can delete this problem")
		}
	}

	if err := l.svcCtx.Repo.DeleteProblem(l.ctx, req.Id); err != nil {
		return nil, err
	}

	if err := packagefs.DeletePackageDir(
		l.svcCtx.Config.Storage.ProblemsRoot,
		p.PackageDir,
	); err != nil {
		return nil, err
	}

	return &types.DeleteProblemResp{
		Deleted: true,
	}, nil
}
