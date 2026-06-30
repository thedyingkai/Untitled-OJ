// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-problem-service/internal/repository"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListProblemsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListProblemsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListProblemsLogic {
	return &ListProblemsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListProblemsLogic) ListProblems(req *types.ListProblemsReq) (resp *types.ListProblemsResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if err := permission.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.view",
		permission.SystemScope(),
	); err != nil {
		return nil, err
	}

	canViewPrivate, err := l.svcCtx.Repo.CanViewPrivateProblems(l.ctx, user.UserID)
	if err != nil {
		return nil, err
	}

	problems, total, err := l.svcCtx.Repo.ListProblems(
		l.ctx,
		repository.ListProblemsFilter{
			UserID:         user.UserID,
			CanViewPrivate: canViewPrivate,
			Page:           req.Page,
			PageSize:       req.PageSize,
			Keyword:        req.Keyword,
			Visibility:     req.Visibility,
			Difficulty:     req.Difficulty,
			Tags:           parseTags(req.Tags),
		},
	)
	if err != nil {
		return nil, err
	}

	items := make([]types.ProblemItem, 0, len(problems))
	for _, p := range problems {
		items = append(items, convertProblem(p))
	}

	return &types.ListProblemsResp{
		Problems: items,
		Total:    total,
	}, nil
}
