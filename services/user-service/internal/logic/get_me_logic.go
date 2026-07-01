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
	userID = strings.TrimSpace(userID)
	if userID == "" {
		userID = "anonymous"
	}
	return profilePtr(l.svcCtx.ProfileStore.GetOrCreateCtx(l.ctx, userID, displayName))
}
