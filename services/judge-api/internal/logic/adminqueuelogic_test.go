package logic

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"

	"ojos-judge-api/internal/types"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
)

func TestAdminQueueReportsRedisStreamLagAndPendingRange(t *testing.T) {
	data, err := os.ReadFile("adminqueuelogic.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"XInfoGroups",
		"ConsumerLag",
		"ConsumerCount",
		"PendingLowestId",
		"PendingHighestId",
		"RedisStatus",
		"XInfoConsumers",
		"XPendingExt",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("AdminQueue must expose Redis Stream lag/pending observability; missing %q", want)
		}
	}
}

func TestAdminQueueRedisStatusReportsConsumersWithMiniredis(t *testing.T) {
	server := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: server.Addr()})
	defer client.Close()

	ctx := context.Background()
	if err := client.XGroupCreateMkStream(ctx, judgeSubmissionStream, judgeConsumerGroup, "0").Err(); err != nil {
		t.Fatalf("create consumer group: %v", err)
	}
	entryID, err := client.XAdd(ctx, &redis.XAddArgs{
		Stream: judgeSubmissionStream,
		Values: judgeTaskEventValues("submission.created", "judge-api-test", 42, time.Now().UTC()),
	}).Result()
	if err != nil {
		t.Fatalf("xadd task: %v", err)
	}
	if _, err := client.XReadGroup(ctx, &redis.XReadGroupArgs{
		Group:    judgeConsumerGroup,
		Consumer: "127.0.0.1_19000_judge-worker",
		Streams:  []string{judgeSubmissionStream, ">"},
		Count:    1,
	}).Result(); err != nil {
		t.Fatalf("xreadgroup task: %v", err)
	}
	if _, err := client.XAdd(ctx, &redis.XAddArgs{
		Stream: judgeResultStream,
		Values: map[string]any{
			"type":    "judge.result.submitted",
			"task_id": "sub-42",
			"status":  "ACCEPTED",
		},
	}).Result(); err != nil {
		t.Fatalf("xadd result: %v", err)
	}

	resp := &types.AdminQueueResp{}
	enrichAdminQueueRedisStatus(ctx, client, resp)

	if resp.TaskStream != judgeSubmissionStream || resp.ResultStream != judgeResultStream || resp.Group != judgeConsumerGroup {
		t.Fatalf("stream identity fields were not populated: %#v", resp)
	}
	if resp.RedisStatus != "ok" {
		t.Fatalf("expected redis status ok, got %#v", resp)
	}
	if resp.StreamLength != 1 || resp.ResultStreamLength != 1 {
		t.Fatalf("unexpected stream lengths: %#v", resp)
	}
	if resp.PendingCount != 1 || resp.PendingLowestId != entryID || resp.PendingHighestId != entryID {
		t.Fatalf("unexpected pending range: %#v", resp)
	}
	if resp.ConsumerCount != 1 || len(resp.Consumers) != 1 {
		t.Fatalf("expected one consumer, got %#v", resp)
	}
	if resp.Consumers[0].Name != "127.0.0.1_19000_judge-worker" || resp.Consumers[0].Pending != 1 {
		t.Fatalf("unexpected consumer details: %#v", resp.Consumers[0])
	}
	if resp.Lag < 0 || resp.ConsumerLag < 0 {
		t.Fatalf("lag should be populated from XINFO GROUPS, got %#v", resp)
	}
}
