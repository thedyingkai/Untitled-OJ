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
	if err := validateWorkerIdentity(l.ctx, workerID); err != nil {
		return nil, err
	}
	req.Status = strings.TrimSpace(strings.ToUpper(req.Status))
	req.Message = strings.TrimSpace(req.Message)
	if err := validateWorkerStatus(req.Status); err != nil {
		return nil, err
	}
	transition, err := successTransition(taskID, workerID, req)
	if err != nil {
		return nil, err
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	lease, err := repo.GetTaskForLease(l.ctx, taskID, workerID, req.LeaseVersion)
	if err != nil {
		if errors.Is(err, repository.ErrTaskLeaseInvalid) {
			return nil, errors.New("task lease is invalid")
		}
		return nil, err
	}
	if lease.Status != "RUNNING" && lease.Status != "SUCCEEDED" {
		return &types.WorkerSubmitResultResp{
			Accepted: false,
			Status:   lease.Status,
		}, nil
	}

	if lease.Status == "RUNNING" {
		// Result artifacts are immutable, but an expired worker must not be able
		// to create unbounded orphan objects before the transactional lease CAS
		// rejects its report. The repository repeats this expiry check while
		// committing the canonical result_path.
		if !lease.LeaseExpiresAt.After(time.Now()) {
			return &types.WorkerSubmitResultResp{Accepted: false, Status: "STALE_LEASE"}, nil
		}
		submission, err := repo.GetSubmission(l.ctx, lease.SubmissionID)
		if err != nil {
			return nil, err
		}
		resultPath, err := stageWorkerResultArtifacts(
			l.ctx,
			l.svcCtx.Config.Storage,
			submission,
			req,
			req.LeaseVersion,
			transition.PayloadSHA256,
		)
		if err != nil {
			return nil, err
		}
		transition.ResultPath = resultPath
	}

	err = repo.MarkTaskSucceeded(
		l.ctx,
		taskID,
		workerID,
		req.LeaseVersion,
		transition,
	)
	alreadySaved := false
	if err != nil {
		if errors.Is(err, repository.ErrTaskTransitionAlreadySaved) {
			alreadySaved = true
		} else {
			if errors.Is(err, repository.ErrTaskLeaseInvalid) {
				return &types.WorkerSubmitResultResp{Accepted: false, Status: "STALE_LEASE"}, nil
			}
			return nil, err
		}
	}
	if err := flushCommittedJudgeResult(l.ctx, l.svcCtx, taskID, workerID, req, alreadySaved); err != nil {
		return nil, err
	}

	return &types.WorkerSubmitResultResp{
		Accepted: true,
		Status:   req.Status,
	}, nil
}
