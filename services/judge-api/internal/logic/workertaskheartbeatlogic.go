package logic

import (
	"context"
	"errors"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type WorkerTaskHeartbeatLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerTaskHeartbeatLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerTaskHeartbeatLogic {
	return &WorkerTaskHeartbeatLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerTaskHeartbeatLogic) WorkerTaskHeartbeat(req *types.WorkerTaskHeartbeatReq) (resp *types.WorkerTaskHeartbeatResp, err error) {
	taskID := strings.TrimSpace(req.TaskId)
	workerID := strings.TrimSpace(req.WorkerId)
	if taskID == "" || workerID == "" || req.LeaseVersion <= 0 {
		return nil, errors.New("invalid task lease")
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	lease, err := repo.RefreshTaskLease(
		l.ctx,
		taskID,
		workerID,
		req.LeaseVersion,
		workerLeaseTTL(l.svcCtx),
	)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return nil, errors.New("task lease is invalid or expired")
		}
		return nil, err
	}

	return &types.WorkerTaskHeartbeatResp{
		TaskId:         lease.TaskID,
		LeaseVersion:   lease.LeaseVersion,
		LeaseExpiresAt: lease.LeaseExpiresAt.UTC().Format(time.RFC3339Nano),
	}, nil
}
