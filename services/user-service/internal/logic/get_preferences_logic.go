// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-user-service/internal/svc"
	"ojos-user-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetPreferencesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetPreferencesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetPreferencesLogic {
	return &GetPreferencesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetPreferencesLogic) GetPreferences(req *types.ProfileReq) (resp *types.PreferencesResp, err error) {
	profile, err := l.svcCtx.ProfileStore.GetOrCreate(req.UserId, req.UserId)
	if err != nil {
		return nil, err
	}
	return &types.PreferencesResp{Preferences: profile.Preferences}, nil
}
