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

type WorkerSubmitResultLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerSubmitResultLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerSubmitResultLogic {
	return &WorkerSubmitResultLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerSubmitResultLogic) WorkerSubmitResult(req *types.WorkerSubmitResultReq) (resp *types.WorkerSubmitResultResp, err error) {
	taskID := strings.TrimSpace(req.TaskId)
	workerID := strings.TrimSpace(req.WorkerId)
	if taskID == "" || workerID == "" || req.LeaseVersion <= 0 {
		return nil, errors.New("invalid task lease")
	}
	req.Status = strings.TrimSpace(strings.ToUpper(req.Status))
	if err := validateWorkerStatus(req.Status); err != nil {
		return nil, err
	}

	lease, err := l.svcCtx.Repo.GetTaskForLease(l.ctx, taskID, workerID, req.LeaseVersion)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return nil, errors.New("task lease is invalid")
		}
		return nil, err
	}
	if lease.Status != "RUNNING" {
		return &types.WorkerSubmitResultResp{
			Accepted: false,
			Status:   lease.Status,
		}, nil
	}

	submission, err := l.svcCtx.Repo.GetSubmission(l.ctx, lease.SubmissionID)
	if err != nil {
		return nil, err
	}
	if err := writeWorkerResultArtifacts(submission, req); err != nil {
		return nil, err
	}

	err = l.svcCtx.Repo.MarkTaskSucceeded(
		l.ctx,
		taskID,
		workerID,
		req.LeaseVersion,
		req.Status,
		req.Score,
		req.TimeMs,
		req.MemoryKb,
		req.Message,
	)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return &types.WorkerSubmitResultResp{Accepted: false, Status: "STALE_LEASE"}, nil
		}
		return nil, err
	}

	return &types.WorkerSubmitResultResp{
		Accepted: true,
		Status:   req.Status,
	}, nil
}
