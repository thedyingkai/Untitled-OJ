// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"strings"

	"ojos-user-service/internal/svc"
	"ojos-user-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetMeLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetMeLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetMeLogic {
	return &GetMeLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetMeLogic) GetMe(userID, displayName string) (resp *types.ProfileResp, err error) {
	contextUserID, contextDisplayName, err := currentUserIDString(l.ctx)
	if err != nil {
		return nil, err
	}
	if err := requireUserProfilePermission(l.ctx, l.svcCtx, contextUserID, "user.profile.read.self", "user.profile.read.any"); err != nil {
		return nil, err
	}
	userID = strings.TrimSpace(userID)
	if userID == "" {
		userID = contextUserID
	}
	displayName = strings.TrimSpace(displayName)
	if displayName == "" {
		displayName = contextDisplayName
	}
	return profilePtr(l.svcCtx.ProfileStore.GetOrCreateCtx(l.ctx, userID, displayName))
}
