// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-api/internal/packagefs"
	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type UpdateProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUpdateProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdateProblemLogic {
	return &UpdateProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdateProblemLogic) UpdateProblem(req *types.UpdateProblemReq) (resp *types.GetProblemResp, err error) {
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
		"problem.edit",
		permission.Scope{Type: "problem", ID: req.Id},
	); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	manifestSha, files, err := packagefs.UpdateManifest(
		p.PackageDir,
		strings.TrimSpace(req.Title),
		strings.TrimSpace(req.Statement),
		strings.TrimSpace(req.ProblemType),
		strings.TrimSpace(req.Visibility),
		strings.TrimSpace(req.Status),
		req.TimeLimitMs,
		req.MemoryLimitMb,
	)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpdateProblem(
		l.ctx,
		req.Id,
		strings.TrimSpace(req.Title),
		strings.TrimSpace(req.Statement),
		strings.TrimSpace(req.ProblemType),
		strings.TrimSpace(req.Visibility),
		strings.TrimSpace(req.Status),
		req.TimeLimitMs,
		req.MemoryLimitMb,
		manifestSha,
	); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, req.Id, files); err != nil {
		return nil, err
	}

	updated, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	return &types.GetProblemResp{
		Problem: convertProblem(*updated),
	}, nil
}
