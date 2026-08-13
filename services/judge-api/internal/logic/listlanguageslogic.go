// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListLanguagesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListLanguagesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListLanguagesLogic {
	return &ListLanguagesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListLanguagesLogic) ListLanguages() (resp *types.ListLanguagesResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}
	checker := l.svcCtx.ActivePermissionChecker()
	if checker == nil {
		return nil, errors.New("permission checker is not configured")
	}
	if err := checker.RequireUserPermission(l.ctx, user.UserID, "judge.submission.view.own", sharedperm.SystemScope()); err != nil {
		return nil, err
	}
	return &types.ListLanguagesResp{
		Languages: convertLanguages(l.svcCtx),
	}, nil
}
