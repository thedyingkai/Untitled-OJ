package logic

import (
	"context"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminWorkersLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminWorkersLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminWorkersLogic {
	return &AdminWorkersLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminWorkersLogic) AdminWorkers() (resp *types.AdminWorkersResp, err error) {
	if err := requireJudgePermission(l.ctx, l.svcCtx, "judge.worker.status"); err != nil {
		return nil, err
	}

	workers, err := l.svcCtx.Repo.ListWorkers(l.ctx, workerLeaseTTL(l.svcCtx)*2)
	if err != nil {
		return nil, err
	}

	items := make([]types.AdminWorkerItem, 0, len(workers))
	for _, w := range workers {
		status := w.Status
		if time.Since(w.LastSeen) > workerLeaseTTL(l.svcCtx)*2 && status != "DRAINING" {
			status = "OFFLINE"
		}
		items = append(items, types.AdminWorkerItem{
			WorkerId:           w.WorkerID,
			WorkerName:         w.WorkerName,
			Hostname:           w.Hostname,
			Version:            w.Version,
			Capabilities:       w.Capabilities,
			SupportedLanguages: w.SupportedLanguages,
			MaxConcurrency:     w.MaxConcurrency,
			RunningCount:       w.RunningCount,
			LastSeen:           w.LastSeen.UTC().Format(time.RFC3339Nano),
			Status:             status,
		})
	}

	return &types.AdminWorkersResp{Workers: items}, nil
}
