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
	if err := validateWorkerIdentity(l.ctx, workerID); err != nil {
		return nil, err
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
	req.Message = message
	resultEvent := workerFailureResultEvent(req, status, message)
	transition, err := failureTransition(
		taskID,
		workerID,
		errorType,
		req.Retryable,
		resultEvent,
	)
	if err != nil {
		return nil, err
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	outcome, err := repo.MarkTaskFailed(
		l.ctx,
		taskID,
		workerID,
		req.LeaseVersion,
		transition,
	)
	alreadySaved := outcome.AlreadySaved
	if err != nil {
		if errors.Is(err, repository.ErrTaskTransitionAlreadySaved) {
			alreadySaved = true
		} else {
			if errors.Is(err, repository.ErrTaskLeaseInvalid) {
				return &types.WorkerFailTaskResp{Accepted: false, Status: "STALE_LEASE"}, nil
			}
			return nil, err
		}
	}

	if outcome.RetryScheduled {
		return &types.WorkerFailTaskResp{Accepted: true, Status: outcome.Status}, nil
	}
	if err := flushCommittedJudgeResult(l.ctx, l.svcCtx, taskID, workerID, resultEvent, alreadySaved); err != nil {
		return nil, err
	}
	return &types.WorkerFailTaskResp{Accepted: true, Status: outcome.Status}, nil
}

func workerFailureResultEvent(req *types.WorkerFailTaskReq, status string, message string) *types.WorkerSubmitResultReq {
	return &types.WorkerSubmitResultReq{
		TaskId:       req.TaskId,
		WorkerId:     req.WorkerId,
		LeaseVersion: req.LeaseVersion,
		Status:       status,
		Score:        0,
		TimeMs:       0,
		MemoryKb:     0,
		Message:      message,
	}
}
