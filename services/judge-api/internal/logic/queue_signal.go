package logic

import (
	"context"
	"strconv"
	"time"

	"ojos-judge-api/internal/svc"

	"github.com/redis/go-redis/v9"
)

const judgeSubmissionStreamMaxLen int64 = 10000

func publishJudgeSignal(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	eventType string,
	producer string,
	submissionID int64,
) error {
	return svcCtx.Redis.XAdd(
		ctx,
		&redis.XAddArgs{
			Stream: judgeSubmissionStream,
			MaxLen: judgeSubmissionStreamMaxLen,
			Approx: true,
			Values: map[string]any{
				"type":          eventType,
				"producer":      producer,
				"submission_id": strconv.FormatInt(submissionID, 10),
				"created_at":    time.Now().UTC().Format(time.RFC3339Nano),
			},
		},
	).Err()
}
