// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListAuditLogsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListAuditLogsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListAuditLogsLogic {
	return &ListAuditLogsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListAuditLogsLogic) ListAuditLogs() (resp *types.ListAuditLogsResp, err error) {
	return NewAdminPermissionsLogic(l.ctx, l.svcCtx).ListAuditLogs()
}
