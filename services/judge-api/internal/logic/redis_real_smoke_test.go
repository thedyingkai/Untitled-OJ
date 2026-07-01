package logic

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/redis/go-redis/v9"
)

func TestRealRedisJudgeTaskStreamSmoke(t *testing.T) {
	redisURL := strings.TrimSpace(os.Getenv("OJOS_REAL_REDIS_URL"))
	if redisURL == "" {
		t.Skip("OJOS_REAL_REDIS_URL is not set")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	options, err := redis.ParseURL(redisURL)
	if err != nil {
		t.Fatalf("parse redis url: %v", err)
	}
	client := redis.NewClient(options)
	defer client.Close()

	stream := judgeSubmissionStream + ":smoke:" + strings.ReplaceAll(time.Now().UTC().Format("20060102150405.000000000"), ".", "")
	group := judgeConsumerGroup
	consumer := "127.0.0.1_19000_judge-worker"

	if err := client.XGroupCreateMkStream(ctx, stream, group, "$").Err(); err != nil {
		t.Fatalf("create consumer group: %v", err)
	}
	entryID, err := client.XAdd(ctx, &redis.XAddArgs{
		Stream: stream,
		Values: judgeTaskEventValues("submission.created", "judge-api-smoke", 4242, time.Now().UTC()),
	}).Result()
	if err != nil {
		t.Fatalf("xadd task: %v", err)
	}
	streams, err := client.XReadGroup(ctx, &redis.XReadGroupArgs{
		Group:    group,
		Consumer: consumer,
		Streams:  []string{stream, ">"},
		Count:    1,
		Block:    time.Second,
	}).Result()
	if err != nil {
		t.Fatalf("xreadgroup task: %v", err)
	}
	if len(streams) != 1 || len(streams[0].Messages) != 1 {
		t.Fatalf("expected one task message, got %#v", streams)
	}
	values := streams[0].Messages[0].Values
	if values["task_id"] != "sub-4242" || values["submission_id"] != "4242" {
		t.Fatalf("unexpected task values: %#v", values)
	}
	acked, err := client.XAck(ctx, stream, group, entryID).Result()
	if err != nil {
		t.Fatalf("xack task: %v", err)
	}
	if acked != 1 {
		t.Fatalf("expected one acked task, got %d", acked)
	}
	pending, err := client.XPending(ctx, stream, group).Result()
	if err != nil {
		t.Fatalf("xpending: %v", err)
	}
	if pending.Count != 0 {
		t.Fatalf("expected no pending tasks after ack, got %d", pending.Count)
	}
	_ = client.Del(ctx, stream).Err()
}
