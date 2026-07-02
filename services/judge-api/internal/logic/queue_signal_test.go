package logic

import (
	"context"
	"testing"
	"time"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
)

func TestJudgeTaskEventValuesDescribeRedisStreamTask(t *testing.T) {
	now := time.Date(2026, 7, 1, 12, 0, 0, 0, time.UTC)
	values := judgeTaskEventValues("submission.created", "judge-api-service", 42, now)

	expected := map[string]string{
		"type":          "submission.created",
		"producer":      "judge-api-service",
		"task_id":       "sub-42",
		"submission_id": "42",
		"created_at":    "2026-07-01T12:00:00Z",
	}
	for key, value := range expected {
		if values[key] != value {
			t.Fatalf("expected %s=%q, got %#v", key, value, values[key])
		}
	}
}

func TestPublishJudgeSignalUsesTaskEventPayload(t *testing.T) {
	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer client.Close()

	err := publishJudgeSignal(context.Background(), &svc.ServiceContext{Redis: client}, "submission.requeued", "judge-api-admin", 42)
	if err != nil {
		t.Fatalf("publishJudgeSignal returned error: %v", err)
	}
	entries, err := client.XRange(context.Background(), judgeSubmissionStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read redis stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one stream entry, got %d", len(entries))
	}
	values := entries[0].Values
	if values["type"] != "submission.requeued" || values["producer"] != "judge-api-admin" {
		t.Fatalf("unexpected event fields: %#v", values)
	}
	if values["task_id"] != "sub-42" || values["submission_id"] != "42" {
		t.Fatalf("task event must include task_id and submission_id, got %#v", values)
	}
	groups, err := client.XInfoGroups(context.Background(), judgeSubmissionStream).Result()
	if err != nil {
		t.Fatalf("read consumer groups: %v", err)
	}
	if len(groups) != 1 || groups[0].Name != judgeConsumerGroup {
		t.Fatalf("expected %s consumer group, got %#v", judgeConsumerGroup, groups)
	}
}

func TestPublishJudgeTaskEventCarriesTraceContext(t *testing.T) {
	otel.SetTextMapPropagator(propagation.TraceContext{})

	spanContext := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    trace.TraceID{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10},
		SpanID:     trace.SpanID{0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18},
		TraceFlags: trace.FlagsSampled,
		Remote:     true,
	})
	ctx := trace.ContextWithRemoteSpanContext(context.Background(), spanContext)

	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer client.Close()

	if err := publishJudgeTaskEvent(ctx, &svc.ServiceContext{Redis: client}, "submission.created", "judge-api-service", 43); err != nil {
		t.Fatalf("publishJudgeTaskEvent returned error: %v", err)
	}
	entries, err := client.XRange(context.Background(), judgeSubmissionStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read redis stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one stream entry, got %d", len(entries))
	}
	if got := entries[0].Values["traceparent"]; got != "00-0102030405060708090a0b0c0d0e0f10-1112131415161718-01" {
		t.Fatalf("task event must carry traceparent, got %#v", entries[0].Values)
	}
}

func TestJudgeResultEventValuesDescribeRedisStreamResult(t *testing.T) {
	now := time.Date(2026, 7, 1, 12, 0, 0, 0, time.UTC)
	values := judgeResultEventValues(
		"sub-42",
		"worker-a",
		&types.WorkerSubmitResultReq{
			LeaseVersion: 3,
			Status:       "ACCEPTED",
			Score:        100,
			TimeMs:       12,
			MemoryKb:     2048,
			Message:      "ok",
		},
		now,
	)

	expected := map[string]string{
		"type":          "judge.result.submitted",
		"producer":      "judge-api-service",
		"task_id":       "sub-42",
		"submission_id": "42",
		"worker_id":     "worker-a",
		"lease_version": "3",
		"status":        "ACCEPTED",
		"score":         "100",
		"time_ms":       "12",
		"memory_kb":     "2048",
		"message":       "ok",
		"created_at":    "2026-07-01T12:00:00Z",
	}
	for key, value := range expected {
		if values[key] != value {
			t.Fatalf("expected %s=%q, got %#v", key, value, values[key])
		}
	}
}

func TestPublishJudgeResultEventWritesRedisResultStream(t *testing.T) {
	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer client.Close()

	err := publishJudgeResultEvent(context.Background(), &svc.ServiceContext{Redis: client}, "sub-42", "worker-a", &types.WorkerSubmitResultReq{
		LeaseVersion: 3,
		Status:       "SYSTEM_ERROR",
		Score:        0,
		TimeMs:       0,
		MemoryKb:     0,
		Message:      "sandbox failed",
	})
	if err != nil {
		t.Fatalf("publishJudgeResultEvent returned error: %v", err)
	}
	entries, err := client.XRange(context.Background(), judgeResultStream, "-", "+").Result()
	if err != nil {
		t.Fatalf("read result stream: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected one result stream entry, got %d", len(entries))
	}
	values := entries[0].Values
	if values["type"] != "judge.result.submitted" || values["task_id"] != "sub-42" {
		t.Fatalf("unexpected result event fields: %#v", values)
	}
	if values["status"] != "SYSTEM_ERROR" || values["worker_id"] != "worker-a" {
		t.Fatalf("unexpected result status fields: %#v", values)
	}
	if values["submission_id"] != "42" || values["message"] != "sandbox failed" {
		t.Fatalf("result event should include submission and message fields: %#v", values)
	}
}

func TestSubmissionIDFromTaskIDOnlyAcceptsDeterministicTaskIDs(t *testing.T) {
	if got := submissionIDFromTaskID("sub-42"); got != "42" {
		t.Fatalf("unexpected submission id %q", got)
	}
	for _, taskID := range []string{"", "42", "sub-", "sub-abc", "task-42"} {
		if got := submissionIDFromTaskID(taskID); got != "" {
			t.Fatalf("task id %q should not produce submission id, got %q", taskID, got)
		}
	}
}
