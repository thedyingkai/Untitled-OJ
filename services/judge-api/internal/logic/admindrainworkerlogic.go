package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminDrainWorkerLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminDrainWorkerLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminDrainWorkerLogic {
	return &AdminDrainWorkerLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminDrainWorkerLogic) AdminDrainWorker(req *types.AdminWorkerActionReq) (resp *types.AdminActionResp, err error) {
	if err := requireJudgeAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	workerID := strings.TrimSpace(req.Id)
	if workerID == "" {
		return nil, errors.New("worker id is required")
	}
	if err := l.svcCtx.Repo.DrainWorker(l.ctx, workerID); err != nil {
		if errors.Is(err, repository.ErrWorkerNotFound) {
			return nil, errors.New("worker not found")
		}
		return nil, err
	}
	return &types.AdminActionResp{Ok: true}, nil
}
