package logic

import (
	"context"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/core/logx"
)

const judgeConsumerGroup = "judge-workers"

type AdminQueueLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminQueueLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminQueueLogic {
	return &AdminQueueLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminQueueLogic) AdminQueue() (resp *types.AdminQueueResp, err error) {
	if err := requireJudgeAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}

	counts, err := l.svcCtx.Repo.QueueTaskCounts(l.ctx)
	if err != nil {
		return nil, err
	}

	resp = &types.AdminQueueResp{
		ConsumerGroup: judgeConsumerGroup,
		TrimStrategy:  "XADD MAXLEN ~ 10000; PostgreSQL judge_tasks is the task ownership source",
		Scheduled:     counts.Scheduled,
		Pending:       counts.Pending,
		Judging:       counts.Judging,
	}

	if info, err := l.svcCtx.Redis.XInfoStream(l.ctx, judgeSubmissionStream).Result(); err == nil {
		resp.StreamLength = info.Length
		resp.LastId = info.LastGeneratedID
	}

	if pending, err := l.svcCtx.Redis.XPending(l.ctx, judgeSubmissionStream, judgeConsumerGroup).Result(); err == nil {
		resp.PendingCount = pending.Count
		if pending.Count > 0 {
			if items, err := l.svcCtx.Redis.XPendingExt(
				l.ctx,
				&redis.XPendingExtArgs{
					Stream: judgeSubmissionStream,
					Group:  judgeConsumerGroup,
					Start:  "-",
					End:    "+",
					Count:  1,
				},
			).Result(); err == nil && len(items) > 0 {
				resp.PendingOldestIdle = items[0].Idle.Milliseconds()
			}
		}
	} else if err != redis.Nil {
		resp.PendingCount = 0
	}

	return resp, nil
}
