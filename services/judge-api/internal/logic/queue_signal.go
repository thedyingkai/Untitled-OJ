package logic

import (
	"context"
	"os"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/redis/go-redis/v9"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
)

const judgeSubmissionStreamMaxLen int64 = 10000
const judgeSubmissionStream = "ojos:judge:task"
const judgeResultStream = "ojos:judge:result"
const judgeConsumerGroup = "judge-worker"

func judgeTaskStreamName() string {
	if value := strings.TrimSpace(os.Getenv("OJOS_JUDGE_TASK_STREAM")); value != "" {
		return value
	}
	return judgeSubmissionStream
}

func judgeResultStreamName() string {
	if value := strings.TrimSpace(os.Getenv("OJOS_JUDGE_RESULT_STREAM")); value != "" {
		return value
	}
	return judgeResultStream
}

func judgeConsumerGroupName() string {
	if value := strings.TrimSpace(os.Getenv("OJOS_JUDGE_CONSUMER_GROUP")); value != "" {
		return value
	}
	return judgeConsumerGroup
}

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
			Stream: judgeTaskStreamName(),
			MaxLen: judgeSubmissionStreamMaxLen,
			Approx: true,
			Values: judgeTaskEventValuesFromContext(ctx, eventType, producer, submissionID, time.Now().UTC()),
		},
	).Err()
}

func ensureJudgeTaskConsumerGroup(ctx context.Context, svcCtx *svc.ServiceContext) error {
	err := svcCtx.Redis.XGroupCreateMkStream(ctx, judgeTaskStreamName(), judgeConsumerGroupName(), "$").Err()
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
			Stream: judgeResultStreamName(),
			MaxLen: judgeSubmissionStreamMaxLen,
			Approx: true,
			Values: judgeResultEventValues(taskID, workerID, req, time.Now().UTC()),
		},
	).Err()
}

func judgeTaskEventValues(eventType string, producer string, submissionID int64, now time.Time) map[string]any {
	return judgeTaskEventValuesFromContext(context.Background(), eventType, producer, submissionID, now)
}

func judgeTaskEventValuesFromContext(ctx context.Context, eventType string, producer string, submissionID int64, now time.Time) map[string]any {
	values := map[string]any{
		"type":          eventType,
		"producer":      producer,
		"task_id":       deterministicTaskIDForStream(submissionID),
		"submission_id": strconv.FormatInt(submissionID, 10),
		"created_at":    now.Format(time.RFC3339Nano),
	}
	injectTraceContext(ctx, values)
	return values
}

func injectTraceContext(ctx context.Context, values map[string]any) {
	carrier := propagation.MapCarrier{}
	otel.GetTextMapPropagator().Inject(ctx, carrier)
	for key, value := range carrier {
		key = strings.ToLower(strings.TrimSpace(key))
		value = strings.TrimSpace(value)
		if key != "" && value != "" {
			values[key] = value
		}
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
