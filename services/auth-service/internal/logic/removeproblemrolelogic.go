// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type RemoveProblemRoleLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewRemoveProblemRoleLogic(ctx context.Context, svcCtx *svc.ServiceContext) *RemoveProblemRoleLogic {
	return &RemoveProblemRoleLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *RemoveProblemRoleLogic) RemoveProblemRole(req *types.ProblemRoleReq) (resp *types.AdminActionResp, err error) {
	return NewAdminPermissionsLogic(l.ctx, l.svcCtx).RemoveProblemRole(req)
}
