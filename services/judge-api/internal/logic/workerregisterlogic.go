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

type WorkerRegisterLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewWorkerRegisterLogic(ctx context.Context, svcCtx *svc.ServiceContext) *WorkerRegisterLogic {
	return &WorkerRegisterLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *WorkerRegisterLogic) WorkerRegister(req *types.WorkerRegisterReq) (resp *types.WorkerRegisterResp, err error) {
	workerID := strings.TrimSpace(req.WorkerId)
	if workerID == "" {
		return nil, errors.New("worker_id is required")
	}
	if err := validateWorkerIdentity(l.ctx, workerID); err != nil {
		return nil, err
	}
	if req.MaxConcurrency <= 0 {
		req.MaxConcurrency = 1
	}
	if req.MaxConcurrency > 64 {
		return nil, errors.New("max_concurrency is too large")
	}

	repo := workerTaskRepo(l.svcCtx)
	if repo == nil {
		return nil, errors.New("worker repository is not configured")
	}

	worker, err := repo.UpsertWorker(l.ctx, repository.WorkerRegistration{
		WorkerID:           workerID,
		WorkerName:         strings.TrimSpace(req.WorkerName),
		Hostname:           strings.TrimSpace(req.Hostname),
		Version:            strings.TrimSpace(req.Version),
		Capabilities:       req.Capabilities,
		SupportedLanguages: req.SupportedLanguages,
		MaxConcurrency:     req.MaxConcurrency,
	})
	if err != nil {
		return nil, err
	}

	return &types.WorkerRegisterResp{
		WorkerId:        worker.WorkerID,
		HeartbeatEveryS: 10,
		LeaseTtlSeconds: int64(workerLeaseTTL(l.svcCtx).Seconds()),
		Status:          worker.Status,
	}, nil
}
