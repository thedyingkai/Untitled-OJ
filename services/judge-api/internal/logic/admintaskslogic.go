package logic

import (
	"context"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminTasksLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminTasksLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminTasksLogic {
	return &AdminTasksLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminTasksLogic) AdminTasks() (resp *types.AdminTasksResp, err error) {
	if err := requireJudgeAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}

	tasks, err := l.svcCtx.Repo.ListTasks(l.ctx, 100)
	if err != nil {
		return nil, err
	}

	items := make([]types.AdminTaskItem, 0, len(tasks))
	for _, task := range tasks {
		items = append(items, types.AdminTaskItem{
			TaskId:         task.TaskID,
			SubmissionId:   task.SubmissionID,
			WorkerId:       task.WorkerID,
			Status:         task.Status,
			LeaseExpiresAt: task.LeaseExpiresAt.UTC().Format(time.RFC3339Nano),
			Attempt:        task.Attempt,
			HeartbeatAt:    task.HeartbeatAt.UTC().Format(time.RFC3339Nano),
		})
	}

	return &types.AdminTasksResp{Tasks: items}, nil
}
