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

type WorkerFailTaskLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerFailTaskLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerFailTaskLogic {
	return &WorkerFailTaskLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerFailTaskLogic) WorkerFailTask(req *types.WorkerFailTaskReq) (resp *types.WorkerFailTaskResp, err error) {
	taskID := strings.TrimSpace(req.TaskId)
	workerID := strings.TrimSpace(req.WorkerId)
	if taskID == "" || workerID == "" || req.LeaseVersion <= 0 {
		return nil, errors.New("invalid task lease")
	}

	status := "SYSTEM_ERROR"
	errorType := strings.ToUpper(strings.TrimSpace(req.ErrorType))
	if errorType == "USER" {
		status = "RUNTIME_ERROR"
		req.Retryable = false
	}
	message := strings.TrimSpace(req.Message)
	if message == "" {
		message = "worker task failed"
	}

	err = l.svcCtx.Repo.MarkTaskFailed(
		l.ctx,
		taskID,
		workerID,
		req.LeaseVersion,
		status,
		message,
		req.Retryable,
	)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return &types.WorkerFailTaskResp{Accepted: false, Status: "STALE_LEASE"}, nil
		}
		return nil, err
	}

	if req.Retryable {
		return &types.WorkerFailTaskResp{Accepted: true, Status: "PENDING"}, nil
	}
	return &types.WorkerFailTaskResp{Accepted: true, Status: status}, nil
}
