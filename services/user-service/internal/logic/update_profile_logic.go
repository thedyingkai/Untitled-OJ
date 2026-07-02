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

type UpdateProfileLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUpdateProfileLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdateProfileLogic {
	return &UpdateProfileLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdateProfileLogic) UpdateProfile(req *types.ProfilePatchReq) (resp *types.ProfileResp, err error) {
	if err := requireUserProfilePermission(l.ctx, l.svcCtx, req.UserId, "user.profile.update.self", "user.profile.update.any"); err != nil {
		return nil, err
	}
	return profilePtr(l.svcCtx.ProfileStore.UpdateCtx(l.ctx, req.UserId, store.ProfilePatch{
		DisplayName:  req.DisplayName,
		Bio:          req.Bio,
		AvatarObject: req.AvatarObject,
		Preferences:  req.Preferences,
	}))
}
