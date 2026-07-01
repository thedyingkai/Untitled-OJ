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

type WorkerClaimTasksLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerClaimTasksLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerClaimTasksLogic {
	return &WorkerClaimTasksLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerClaimTasksLogic) WorkerClaimTasks(req *types.WorkerClaimTasksReq) (resp *types.WorkerClaimTasksResp, err error) {
	workerID := strings.TrimSpace(req.WorkerId)
	if workerID == "" {
		return nil, errors.New("worker_id is required")
	}
	if req.AvailableSlots <= 0 {
		return &types.WorkerClaimTasksResp{Tasks: []types.WorkerTaskLease{}}, nil
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	if _, err := repo.RecoverStaleTasks(l.ctx); err != nil {
		return nil, err
	}

	leases, err := repo.ClaimTasks(
		l.ctx,
		workerID,
		req.SupportedLanguages,
		req.AvailableSlots,
		workerLeaseTTL(l.svcCtx),
		normalizeWorkerTaskIDs(req.TaskIds),
	)
	if err != nil {
		if errors.Is(err, repository.ErrWorkerNotFound) {
			return nil, errors.New("worker is not registered")
		}
		if errors.Is(err, repository.ErrWorkerDraining) {
			return &types.WorkerClaimTasksResp{Tasks: []types.WorkerTaskLease{}}, nil
		}
		return nil, err
	}

	items := make([]types.WorkerTaskLease, 0, len(leases))
	for i := range leases {
		item, err := taskLeaseToResp(l.ctx, l.svcCtx, &leases[i])
		if err != nil {
			return nil, err
		}
		items = append(items, item)
	}

	return &types.WorkerClaimTasksResp{Tasks: items}, nil
}
