// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-user-service/internal/svc"
	"ojos-user-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetStatsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetStatsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetStatsLogic {
	return &GetStatsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetStatsLogic) GetStats(req *types.ProfileReq) (resp *types.UserStats, err error) {
	if err := requireUserProfilePermission(l.ctx, l.svcCtx, req.UserId, "user.stats.read", "user.stats.read"); err != nil {
		return nil, err
	}
	profile, err := l.svcCtx.ProfileStore.GetOrCreateCtx(l.ctx, req.UserId, req.UserId)
	if err != nil {
		return nil, err
	}
	return &profile.Stats, nil
}
