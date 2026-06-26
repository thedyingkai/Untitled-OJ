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

type WorkerHeartbeatLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerHeartbeatLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerHeartbeatLogic {
	return &WorkerHeartbeatLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerHeartbeatLogic) WorkerHeartbeat(req *types.WorkerHeartbeatReq) (resp *types.WorkerHeartbeatResp, err error) {
	workerID := strings.TrimSpace(req.WorkerId)
	if workerID == "" {
		return nil, errors.New("worker_id is required")
	}
	if req.RunningCount < 0 {
		return nil, errors.New("running_count is invalid")
	}

	worker, err := l.svcCtx.Repo.WorkerHeartbeat(l.ctx, workerID, req.RunningCount)
	if err != nil {
		if errors.Is(err, repository.ErrWorkerNotFound) {
			return nil, errors.New("worker is not registered")
		}
		return nil, err
	}

	return &types.WorkerHeartbeatResp{
		WorkerId: worker.WorkerID,
		Status:   worker.Status,
	}, nil
}
