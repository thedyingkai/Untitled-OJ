// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-user-service/internal/store"
	"ojos-user-service/internal/svc"
	"ojos-user-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type UpdatePreferencesLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUpdatePreferencesLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdatePreferencesLogic {
	return &UpdatePreferencesLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdatePreferencesLogic) UpdatePreferences(req *types.ProfilePatchReq) (resp *types.ProfileResp, err error) {
	return profilePtr(l.svcCtx.ProfileStore.Update(req.UserId, store.ProfilePatch{
		Preferences: req.Preferences,
	}))
}
