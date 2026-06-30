// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminOrchestratorSnapshotLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminOrchestratorSnapshotLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminOrchestratorSnapshotLogic {
	return &AdminOrchestratorSnapshotLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminOrchestratorSnapshotLogic) AdminOrchestratorSnapshot(req *types.AdminSnapshotReq) (resp *types.OrchestratorSnapshotResp, err error) {
	return NewAdminServicesLogic(l.ctx, l.svcCtx).OrchestratorSnapshot(req.Authorization, req.IncludeDisabled)
}
