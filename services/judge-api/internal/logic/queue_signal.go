package logic

import (
	"context"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/redis/go-redis/v9"
)

const judgeSubmissionStreamMaxLen int64 = 10000
const judgeResultStream = "ojos:judge:results"
const judgeConsumerGroup = "judge-workers"

func publishJudgeSignal(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	eventType string,
	producer string,
	submissionID int64,
) error {
	return publishJudgeTaskEvent(ctx, svcCtx, eventType, producer, submissionID)
}

func publishJudgeTaskEvent(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	eventType string,
	producer string,
	submissionID int64,
) error {
	if err := ensureJudgeTaskConsumerGroup(ctx, svcCtx); err != nil {
		return err
	}
	return svcCtx.Redis.XAdd(
		ctx,
		&redis.XAddArgs{
			Stream: judgeSubmissionStream,
			MaxLen: judgeSubmissionStreamMaxLen,
			Approx: true,
			Values: judgeTaskEventValues(eventType, producer, submissionID, time.Now().UTC()),
		},
	).Err()
}

func ensureJudgeTaskConsumerGroup(ctx context.Context, svcCtx *svc.ServiceContext) error {
	err := svcCtx.Redis.XGroupCreateMkStream(ctx, judgeSubmissionStream, judgeConsumerGroup, "$").Err()
	if err == nil || strings.Contains(err.Error(), "BUSYGROUP") {
		return nil
	}
	return err
}

func publishJudgeResultEvent(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	taskID string,
	workerID string,
	req *types.WorkerSubmitResultReq,
) error {
	return svcCtx.Redis.XAdd(
		ctx,
		&redis.XAddArgs{
			Stream: judgeResultStream,
			MaxLen: judgeSubmissionStreamMaxLen,
			Approx: true,
			Values: judgeResultEventValues(taskID, workerID, req, time.Now().UTC()),
		},
	).Err()
}

func judgeTaskEventValues(eventType string, producer string, submissionID int64, now time.Time) map[string]any {
	return map[string]any{
		"type":          eventType,
		"producer":      producer,
		"task_id":       deterministicTaskIDForStream(submissionID),
		"submission_id": strconv.FormatInt(submissionID, 10),
		"created_at":    now.Format(time.RFC3339Nano),
	}
}

func judgeResultEventValues(
	taskID string,
	workerID string,
	req *types.WorkerSubmitResultReq,
	now time.Time,
) map[string]any {
	values := map[string]any{
		"type":          "judge.result.submitted",
		"producer":      "judge-api-service",
		"task_id":       taskID,
		"worker_id":     workerID,
		"lease_version": strconv.Itoa(req.LeaseVersion),
		"status":        req.Status,
		"score":         strconv.Itoa(req.Score),
		"time_ms":       strconv.Itoa(req.TimeMs),
		"memory_kb":     strconv.Itoa(req.MemoryKb),
		"message":       req.Message,
		"created_at":    now.Format(time.RFC3339Nano),
	}
	if submissionID := submissionIDFromTaskID(taskID); submissionID != "" {
		values["submission_id"] = submissionID
	}
	return values
}

func submissionIDFromTaskID(taskID string) string {
	taskID = strings.TrimSpace(taskID)
	if !strings.HasPrefix(taskID, "sub-") {
		return ""
	}
	submissionID := strings.TrimPrefix(taskID, "sub-")
	if submissionID == "" {
		return ""
	}
	if _, err := strconv.ParseInt(submissionID, 10, 64); err != nil {
		return ""
	}
	return submissionID
}

func deterministicTaskIDForStream(submissionID int64) string {
	return "sub-" + strconv.FormatInt(submissionID, 10)
}
