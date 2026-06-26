// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"ojos-auth/internal/middleware"

	"ojos-auth/internal/svc"
	"ojos-auth/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ProfileLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewProfileLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ProfileLogic {
	return &ProfileLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ProfileLogic) Profile() (resp *types.ProfileResp, err error) {
	claims, ok := middleware.ClaimsFromContext(l.ctx)
	if !ok || claims == nil {
		return &types.ProfileResp{
			Code: 40105,
			Msg:  "unauthorized",
		}, nil
	}

	roles, err := l.svcCtx.UserRepo.GetRolesByUserID(l.ctx, claims.UserID)
	if err != nil {
		return nil, err
	}

	permissions, err := l.svcCtx.UserRepo.GetPermissionCodesByUserID(l.ctx, claims.UserID)
	if err != nil {
		return nil, err
	}

	return &types.ProfileResp{
		Code: 0,
		Msg:  "success",
		Data: types.ProfileData{
			UserId:      claims.UserID,
			Username:    claims.Username,
			Roles:       roles,
			Permissions: permissions,
		},
	}, nil
}
