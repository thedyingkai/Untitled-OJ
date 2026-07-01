package types

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestAdminQueueRespExposesRedisStreamObservabilityFields(t *testing.T) {
	payload, err := json.Marshal(AdminQueueResp{
		ConsumerCount:    2,
		ConsumerLag:      7,
		PendingLowestId:  "1-0",
		PendingHighestId: "8-0",
		RedisStatus:      "ok",
	})
	if err != nil {
		t.Fatal(err)
	}
	text := string(payload)
	for _, want := range []string{
		"consumer_count",
		"consumer_lag",
		"pending_lowest_id",
		"pending_highest_id",
		"redis_status",
	} {
		if !strings.Contains(text, `"`+want+`":`) {
			t.Fatalf("AdminQueueResp JSON must expose %s, got %s", want, text)
		}
	}
}

func TestWorkerClaimTasksReqExposesStreamTaskIds(t *testing.T) {
	payload, err := json.Marshal(WorkerClaimTasksReq{
		WorkerId:       "worker-a",
		AvailableSlots: 1,
		TaskIds:        []string{"sub-42"},
	})
	if err != nil {
		t.Fatal(err)
	}
	text := string(payload)
	if !strings.Contains(text, `"task_ids":["sub-42"]`) {
		t.Fatalf("WorkerClaimTasksReq JSON must expose task_ids, got %s", text)
	}
}
