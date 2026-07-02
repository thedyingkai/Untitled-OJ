package logic

import (
	"context"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/core/logx"
)

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

	var scheduled int64
	var pending int64
	var judging int64
	if l.svcCtx.Repo != nil {
		counts, err := l.svcCtx.Repo.QueueTaskCounts(l.ctx)
		if err != nil {
			return nil, err
		}
		scheduled = counts.Scheduled
		pending = counts.Pending
		judging = counts.Judging
	}

	resp = &types.AdminQueueResp{
		TaskStream:    judgeTaskStreamName(),
		ResultStream:  judgeResultStreamName(),
		Group:         judgeConsumerGroupName(),
		ConsumerGroup: judgeConsumerGroupName(),
		ConsumerLag:   -1,
		Lag:           -1,
		RedisStatus:   "unavailable",
		TrimStrategy:  "XADD MAXLEN ~ 10000; PostgreSQL judge_tasks is the task ownership source",
		Scheduled:     scheduled,
		Pending:       pending,
		Judging:       judging,
	}

	enrichAdminQueueRedisStatus(l.ctx, l.svcCtx.Redis, resp)
	return resp, nil
}

func enrichAdminQueueRedisStatus(ctx context.Context, client *redis.Client, resp *types.AdminQueueResp) {
	if resp == nil {
		return
	}
	if resp.TaskStream == "" {
		resp.TaskStream = judgeTaskStreamName()
	}
	if resp.ResultStream == "" {
		resp.ResultStream = judgeResultStreamName()
	}
	if resp.Group == "" {
		resp.Group = judgeConsumerGroupName()
	}
	if resp.ConsumerGroup == "" {
		resp.ConsumerGroup = judgeConsumerGroupName()
	}
	if resp.ConsumerLag == 0 && resp.Lag == 0 {
		resp.ConsumerLag = -1
		resp.Lag = -1
	}
	if resp.RedisStatus == "" {
		resp.RedisStatus = "unavailable"
	}
	if client == nil {
		return
	}

	if info, err := client.XInfoStream(ctx, resp.TaskStream).Result(); err == nil {
		resp.StreamLength = info.Length
		resp.LastId = info.LastGeneratedID
		resp.RedisStatus = "ok"
	}
	if info, err := client.XInfoStream(ctx, resp.ResultStream).Result(); err == nil {
		resp.ResultStreamLength = info.Length
		resp.ResultLastId = info.LastGeneratedID
		resp.RedisStatus = "ok"
	}
	if groups, err := client.XInfoGroups(ctx, resp.TaskStream).Result(); err == nil {
		resp.RedisStatus = "ok"
		for _, group := range groups {
			if group.Name != resp.Group {
				continue
			}
			resp.ConsumerCount = group.Consumers
			resp.ConsumerLag = group.Lag
			resp.Lag = group.Lag
			if group.Pending > resp.PendingCount {
				resp.PendingCount = group.Pending
			}
			break
		}
	}

	if consumers, err := client.XInfoConsumers(ctx, resp.TaskStream, resp.Group).Result(); err == nil {
		resp.RedisStatus = "ok"
		resp.Consumers = make([]types.AdminQueueConsumer, 0, len(consumers))
		for _, consumer := range consumers {
			resp.Consumers = append(resp.Consumers, types.AdminQueueConsumer{
				Name:       consumer.Name,
				Pending:    consumer.Pending,
				IdleMs:     consumer.Idle.Milliseconds(),
				InactiveMs: consumer.Inactive.Milliseconds(),
			})
		}
		if resp.ConsumerCount == 0 {
			resp.ConsumerCount = int64(len(resp.Consumers))
		}
	}

	if pending, err := client.XPending(ctx, resp.TaskStream, resp.Group).Result(); err == nil {
		resp.RedisStatus = "ok"
		resp.PendingCount = pending.Count
		resp.PendingLowestId = pending.Lower
		resp.PendingHighestId = pending.Higher
		if pending.Count > 0 {
			if items, err := client.XPendingExt(
				ctx,
				&redis.XPendingExtArgs{
					Stream: resp.TaskStream,
					Group:  resp.Group,
					Start:  "-",
					End:    "+",
					Count:  1,
				},
			).Result(); err == nil && len(items) > 0 {
				resp.PendingOldestIdle = items[0].Idle.Milliseconds()
			}
		}
	} else if err != redis.Nil {
		if resp.RedisStatus == "ok" {
			resp.RedisStatus = "partial"
		}
	}
}
