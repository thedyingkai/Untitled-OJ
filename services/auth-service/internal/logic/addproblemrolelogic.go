// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AddProblemRoleLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAddProblemRoleLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AddProblemRoleLogic {
	return &AddProblemRoleLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AddProblemRoleLogic) AddProblemRole(req *types.ProblemRoleReq) (resp *types.AdminActionResp, err error) {
	return NewAdminPermissionsLogic(l.ctx, l.svcCtx).AddProblemRole(req)
}
